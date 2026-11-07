#!/usr/bin/env python3
"""Drive a host over its RPC protocol with scripted dialog answers.

usage: rpcdrive.py <steps.json> <prefix> <timeout> -- <argv...>

steps.json:
  {"answers": {"select": int|cancel, "input": "text"},
   "steps": [{"cmd": {...}, "wait": "settled"|"response"|"none"}, ...]}

`answers` decides how every `extension_ui_request` is answered: `select` takes an option index
(or is cancelled), `input` types text. Each step writes `cmd` and waits for `settled` (an
`agent_settled` event), `response` (a `response` event) or nothing.

Every protocol record is appended to `<prefix>.events.jsonl` as `{"t": seconds, "rec": {...}}`;
the last line of stdout is `{"status": "ok"|"failed", ...}`. The host's stderr goes to
`<prefix>.stderr`.
"""
import json, subprocess, sys, threading, time

steps = json.load(open(sys.argv[1]))
prefix, timeout = sys.argv[2], float(sys.argv[3])
argv = sys.argv[sys.argv.index("--") + 1:]
rules = steps.get("answers", {}) if isinstance(steps, dict) else {}
steps = steps["steps"] if isinstance(steps, dict) else steps
ev_log = open(prefix + ".events.jsonl", "w", buffering=1)
err = open(prefix + ".stderr", "w")
p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err)
events, cond = [], threading.Condition()

def send(obj):
    p.stdin.write((json.dumps(obj) + "\n").encode()); p.stdin.flush()

def answer(rec):
    m, rid, opts = rec.get("method"), rec.get("id"), rec.get("options") or []
    choice = rules.get("select")
    if m == "select":
        if choice == "cancel":
            send({"type": "extension_ui_response", "id": rid, "cancelled": True})
        elif isinstance(choice, int):
            send({"type": "extension_ui_response", "id": rid, "value": opts[choice] if len(opts) > choice else None})
        else:
            send({"type": "extension_ui_response", "id": rid, "value": opts[0] if opts else None})
    elif m == "confirm":
        send({"type": "extension_ui_response", "id": rid, "confirmed": choice != "cancel"})
    elif m in ("input", "editor"):
        send({"type": "extension_ui_response", "id": rid,
              "value": None if choice == "cancel" else (rules.get("input") or "test")})

def reader():
    buf = b""
    while True:
        chunk = p.stdout.read1(65536)
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                rec = {"_raw": line.decode("utf-8", "replace")}
            ev_log.write(json.dumps({"t": time.time(), "rec": rec}) + "\n")
            if rec.get("type") == "extension_ui_request":
                answer(rec)
            with cond:
                events.append(rec); cond.notify_all()
    with cond:
        events.append({"type": "_eof"}); cond.notify_all()

threading.Thread(target=reader, daemon=True).start()

def wait(kind, limit):
    deadline = time.time() + limit
    start = len(events)
    with cond:
        while time.time() < deadline:
            seen = events[start:]
            if kind == "settled" and any(e.get("type") == "agent_settled" for e in seen):
                return True
            if kind == "response" and any(e.get("type") == "response" for e in seen):
                return True
            if any(e.get("type") == "_eof" for e in events):
                return False
            cond.wait(0.2)
    return False

ok = True
for step in steps:
    send(step["cmd"])
    kind = step.get("wait", "response")
    if kind == "none":
        time.sleep(step.get("sleep", 0.2))
    elif not wait(kind, step.get("timeout", timeout)):
        ok = False
        print("step timed out:", step["cmd"], file=sys.stderr)
        break

p.stdin.close()
try:
    p.wait(timeout=60)
except subprocess.TimeoutExpired:
    p.kill(); ok = False
with cond:
    deadline = time.time() + 5
    while time.time() < deadline and not any(e.get("type") == "_eof" for e in events):
        cond.wait(0.2)
print(json.dumps({"status": "ok" if ok else "failed", "exit": p.returncode, "events": len(events)}))
