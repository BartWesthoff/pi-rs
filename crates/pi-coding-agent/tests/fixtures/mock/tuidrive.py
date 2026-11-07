#!/usr/bin/env python3
"""Drive interactive hosts in a private tmux server and capture the rendered screens.

usage: tuidrive.py <plan.json> <result.json>

plan.json:
  {"cols": 120, "rows": 40, "runs": [{"argv": [...], "steps": [...]}, ...]}

Runs execute one after another, each in a fresh tmux session whose pane runs `argv` with this
script's environment. Steps:
  {"wait": "<substring>", "timeout": s}       until the screen contains the substring
  {"wait_gone": "<substring>", "timeout": s}  until it no longer does
  {"keys": ["Escape", ...]}                   tmux key names
  {"keys": [...], "until": "<substring>", "tries": n, "timeout": s}
                                              resend the keys until the screen contains the
                                              substring (a completion popup may take one Enter)
  {"text": "literal"}                         typed literally
  {"snap": "name"}                            record the screen once it has settled
  {"sleep": s}
  {"exit": s}                                 until the pane's command has exited

result.json: {"runs": [{"ok": bool, "failed": "<step>"|null, "snaps": {name: text},
              "exit": "<status>"|null, "screen": "<last screen>"}]}

A run stops at its first failed wait; later runs still execute. The tmux server uses a private
socket in a fresh short directory under /tmp (socket paths are limited to 108 bytes) and is
killed at the end.
"""
import json
import shutil
import subprocess
import sys
import tempfile
import time

plan = json.load(open(sys.argv[1]))
result_path = sys.argv[2]
SOCKET_DIR = tempfile.mkdtemp(prefix="pkg-tmux-", dir="/tmp")
CONFIG = SOCKET_DIR + "/tmux.conf"
# Pi nudges when tmux leaves extended keys off; setting them keeps the warning out of both hosts'
# screens so a snapshot compares package UI rather than host chrome.
open(CONFIG, "w").write("set -g extended-keys on\nset -g extended-keys-format csi-u\n")
TMUX = ["tmux", "-S", SOCKET_DIR + "/s", "-f", CONFIG]
SESSION = "host"
SETTLE = 0.4


def tmux(*args, check=True):
    done = subprocess.run(TMUX + list(args), capture_output=True, text=True)
    if check and done.returncode != 0:
        sys.exit(f"tmux {' '.join(args[:1])} failed ({done.returncode}): {done.stderr.strip()}")
    return done


def screen():
    return tmux("capture-pane", "-p", "-t", SESSION, check=False).stdout


def settled(timeout=5.0):
    """The screen once it has not changed for SETTLE seconds."""
    deadline = time.time() + timeout
    last = screen()
    stable_since = time.time()
    while time.time() < deadline:
        time.sleep(0.1)
        now = screen()
        if now != last:
            last, stable_since = now, time.time()
        elif time.time() - stable_since >= SETTLE:
            break
    return last


def pane_exit():
    out = tmux("display-message", "-p", "-t", SESSION, "#{pane_dead} #{pane_dead_status}", check=False).stdout.split()
    return out[1] if len(out) == 2 and out[0] == "1" else None


def wait_for(predicate, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return predicate()


def run_one(run):
    tmux("kill-server", check=False)
    tmux("new-session", "-d", "-s", SESSION, "-x", str(plan.get("cols", 120)), "-y", str(plan.get("rows", 40)),
         "--", *run["argv"])
    tmux("set-option", "-t", SESSION, "remain-on-exit", "on")
    out = {"ok": True, "failed": None, "snaps": {}, "exit": None}
    for step in run["steps"]:
        timeout = step.get("timeout", 60)
        if "until" in step:
            ok = False
            for _ in range(step.get("tries", 3)):
                tmux("send-keys", "-t", SESSION, *step["keys"])
                if wait_for(lambda: step["until"] in screen(), step.get("timeout", 3)):
                    ok = True
                    break
        elif "wait" in step:
            ok = wait_for(lambda: step["wait"] in screen(), timeout)
        elif "wait_gone" in step:
            ok = wait_for(lambda: step["wait_gone"] not in screen(), timeout)
        elif "exit" in step:
            ok = wait_for(lambda: pane_exit() is not None, step["exit"])
        else:
            ok = True
            if "keys" in step:
                tmux("send-keys", "-t", SESSION, *step["keys"])
            elif "text" in step:
                tmux("send-keys", "-t", SESSION, "-l", step["text"])
            elif "snap" in step:
                out["snaps"][step["snap"]] = settled()
            elif "sleep" in step:
                time.sleep(step["sleep"])
        if not ok:
            out["ok"] = False
            out["failed"] = json.dumps(step)
            break
    out["screen"] = screen()
    out["exit"] = pane_exit()
    tmux("kill-server", check=False)
    return out


try:
    results = {"runs": [run_one(run) for run in plan["runs"]]}
finally:
    tmux("kill-server", check=False)
    shutil.rmtree(SOCKET_DIR, ignore_errors=True)
json.dump(results, open(result_path, "w"), indent=2)
