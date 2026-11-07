#!/usr/bin/env bash
# Startup and idle footprint of the interactive `pi` in a pseudo-terminal (PERF-05).
#
# For each command it records, over RUNS runs (median printed):
#   first paint  time from spawn to the first byte written to the terminal
#   ready        time until the editor frame shows (the footer's "(auto)" gauge)
#   idle RSS     VmRSS of the process tree IDLE_SECONDS after ready, and VmHWM (peak)
#   threads      Threads of the process tree at the same moment
#
# Every run uses a fresh, empty HOME and PI_CODING_AGENT_DIR (no auth, no models, no packages)
# and PI_OFFLINE=1, so nothing from the user's own setup is read and no network is needed.
#
# Usage:
#   scripts/perf/run.sh [target/release/pi]
#   PI_PERF_TS="node /path/to/pi/packages/coding-agent/dist/cli.js" scripts/perf/run.sh target/release/pi
#     also measures TypeScript Pi side by side (same isolation)
# Environment: RUNS (default 7), IDLE_SECONDS (default 3), COLUMNS/LINES (default 120x40),
#   PI_PERF_JSON=<file> to also write the raw runs as JSON.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
binary="${1:-$root/target/release/pi}"
if [[ ! -x "$binary" ]]; then
	echo "no executable at $binary; build it with: cargo build --release --locked -p pi-coding-agent --bin pi" >&2
	exit 1
fi

exec python3 -I - "$binary" <<'PY'
import json, os, pty, select, shlex, signal, statistics, sys, tempfile, time

RUNS = int(os.environ.get("RUNS", "7"))
IDLE = float(os.environ.get("IDLE_SECONDS", "3"))
COLS = int(os.environ.get("COLUMNS", "120"))
ROWS = int(os.environ.get("LINES", "40"))
READY_MARK = b"(auto)"
TIMEOUT = 30.0

commands = [("pi-rs", [sys.argv[1]])]
if os.environ.get("PI_PERF_TS"):
    commands.append(("pi-ts", shlex.split(os.environ["PI_PERF_TS"])))


def tree(pid):
    """pid and all its descendants, from /proc."""
    children = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
        except OSError:
            continue
        children.setdefault(ppid, []).append(int(entry))
    out, stack = [], [pid]
    while stack:
        p = stack.pop()
        out.append(p)
        stack.extend(children.get(p, []))
    return out


def status(pids):
    totals = {"VmRSS": 0, "VmHWM": 0, "Threads": 0}
    for p in pids:
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    key, _, value = line.partition(":")
                    if key in totals:
                        totals[key] += int(value.split()[0])
        except OSError:
            pass
    return totals


def run_once(argv):
    with tempfile.TemporaryDirectory(prefix="pi-perf-") as home:
        agent = os.path.join(home, "agent")
        work = os.path.join(home, "work")
        os.makedirs(agent)
        os.makedirs(work)
        env = {
            "HOME": home,
            "PI_CODING_AGENT_DIR": agent,
            "PI_OFFLINE": "1",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "TERM": "xterm-256color",
            "COLUMNS": str(COLS),
            "LINES": str(ROWS),
            "LANG": os.environ.get("LANG", "C.UTF-8"),
        }
        started = time.monotonic()
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(work)
            os.execvpe(argv[0], argv, env)
        import fcntl, struct, termios
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        first = ready = None
        seen = b""
        try:
            while time.monotonic() - started < TIMEOUT:
                readable, _, _ = select.select([fd], [], [], 0.005)
                if not readable:
                    continue
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    break
                if not data:
                    break
                now = time.monotonic()
                if first is None:
                    first = now - started
                seen = (seen + data)[-4096:]
                if READY_MARK in seen:
                    ready = now - started
                    break
            if ready is None:
                raise RuntimeError(f"{argv[0]} did not show the editor within {TIMEOUT}s")
            # Keep draining output while idle so the program never blocks on a full terminal.
            deadline = time.monotonic() + IDLE
            while time.monotonic() < deadline:
                if select.select([fd], [], [], 0.05)[0]:
                    try:
                        os.read(fd, 65536)
                    except OSError:
                        break
            footprint = status(tree(pid))
        finally:
            for p in reversed(tree(pid)):
                try:
                    os.kill(p, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.waitpid(pid, 0)
            os.close(fd)
        return {
            "firstPaintMs": first * 1000,
            "readyMs": ready * 1000,
            "idleRssKiB": footprint["VmRSS"],
            "peakRssKiB": footprint["VmHWM"],
            "threads": footprint["Threads"],
        }


results = {}
for name, argv in commands:
    run_once(argv)  # warm-up: page cache, lazy files
    results[name] = [run_once(argv) for _ in range(RUNS)]

fields = [("first paint", "firstPaintMs", "ms"), ("ready", "readyMs", "ms"),
          ("idle RSS", "idleRssKiB", "MiB"), ("peak RSS", "peakRssKiB", "MiB"), ("threads", "threads", "")]
names = list(results)
print(f"median of {RUNS} runs, {COLS}x{ROWS} pty, idle sampled {IDLE:g}s after ready")
print(f"{'':<12}" + "".join(f"{n:>12}" for n in names))
for label, key, unit in fields:
    row = f"{label:<12}"
    for n in names:
        value = statistics.median(r[key] for r in results[n])
        if unit == "MiB":
            row += f"{value / 1024:>8.1f} MiB"
        elif unit == "ms":
            row += f"{value:>9.1f} ms"
        else:
            row += f"{value:>12.0f}"
    print(row)
if os.environ.get("PI_PERF_JSON"):
    with open(os.environ["PI_PERF_JSON"], "w") as f:
        json.dump(results, f, indent=2)
PY
