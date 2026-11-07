#!/usr/bin/env python3
"""Startup and memory footprint of unchanged Pi packages (PKGP-02, subset of PERF-05).

For three configurations - no packages, each fixture package alone, and the full set - this
measures pi-rs and TS Pi 1.4.0 side by side:

  ready        time from spawn to the first prompt-ready editor frame (the footer's "(auto)")
  timings      the PI_TIMING=1 total from `core/timings.rs` (TS: `core/timings.ts`)
  idle RSS     VmRSS of the whole process tree IDLE_SECONDS after ready
  peak RSS     VmHWM of the whole process tree, pi-rs and spawned children apart
  children     number of child processes at that moment (bili proxy, context-mode server, ...)
  compiled JS  JavaScript source bytes handed to the JS compiler (see below)

Every run uses a fresh HOME and PI_CODING_AGENT_DIR built from the PKGP-01 fixture cache, with
PI_OFFLINE=1: nothing from the user's own setup is read and no network is needed.

Compiled JS is counted differently per host because they compile differently: pi-rs reports its
QuickJS byte count through PI_TIMING (`Compiled JS: N bytes`); Node cannot report its own, so TS
Pi runs with `--require js-compile-counter.cjs`, which wraps CommonJS `_compile`, the ESM
`module.registerHooks` chain and the `vm` compilers (jiti/Babel transpile through `vm`).

Usage:
  scripts/package-fixtures.sh                     # once: fetch the pinned fixture packages
  cargo build --release --locked -p pi-coding-agent --bin pi
  scripts/perf/packages.py                        # both hosts, all configurations

  scripts/perf/packages.py --host pi-rs            # one host
  scripts/perf/packages.py --runs 3 --config all   # one configuration
  scripts/perf/packages.py --markdown docs/perf/packages.md
  scripts/perf/packages.py --json out.json
  scripts/perf/packages.py --self-test             # empty set, RUNS=1, assert sane numbers

Budgets: when `perf/budgets.toml` exists, a median above a budget makes the script exit 1. The
file is a TOML table of metric names, either global (`[default]`) or per configuration
(`["pi-rs:all"]`, `["pi-ts:context-mode"]`). Metrics: ready_ms, idle_rss_kib, peak_rss_kib,
children, compiled_js_bytes. PKGP-22 sets the real numbers.

Environment: PI_PERF_TS="node /path/to/pi/packages/coding-agent/dist/bundle/cli.js" selects TS Pi,
PI_RS_PKG_FIXTURES_DIR overrides the fixture cache, RUNS / IDLE_SECONDS / COLUMNS / LINES behave as
in scripts/perf/run.sh.
"""

import argparse
import fcntl
import glob
import json
import os
import pty
import select
import shutil
import signal
import statistics
import struct
import sys
import tempfile
import termios
import time
import tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
COUNTER = os.path.join(HERE, "js-compile-counter.cjs")
SETTINGS_FIXTURE = os.path.join(
    REPO, "crates/pi-coding-agent/tests/fixtures/packages/settings.json"
)
READY_MARK = b"(auto)"
TIMEOUT = 120.0


def log(message):
    print(message, file=sys.stderr, flush=True)


# ---------------------------------------------------------------- process tree / memory


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


def footprint(pids):
    """Summed VmRSS/VmHWM/Threads over pids, plus a per-process breakdown."""
    total = {"VmRSS": 0, "VmHWM": 0, "Threads": 0}
    processes = []
    for p in pids:
        stats = {}
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    key, _, value = line.partition(":")
                    if key in total:
                        value = int(value.split()[0])
                        stats[key] = value
                        total[key] += value
            with open(f"/proc/{p}/cmdline", "rb") as f:
                cmdline = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
        except OSError:
            continue
        processes.append({"pid": p, "cmdline": cmdline[:200], **stats})
    return total, processes


# ---------------------------------------------------------------- sandbox


def copy_tree(source, target):
    """Copy the fixture agent dir, reflinking when the filesystem can."""
    os.makedirs(target, exist_ok=True)
    copied = os.system(
        f"cp -a --reflink=auto {source.rstrip('/')}/. {target}/ 2>/dev/null"
    )
    if copied != 0:
        shutil.copytree(source, target, dirs_exist_ok=True, symlinks=True)


def make_sandbox(root, fixtures, packages):
    """Fresh HOME + PI_CODING_AGENT_DIR with the fixture packages and the wanted settings."""
    home = os.path.join(root, "home")
    agent = os.path.join(home, ".pi/agent")
    work = os.path.join(root, "work")
    tmp = os.path.join(root, "tmp")
    for d in (home, agent, work, tmp):
        os.makedirs(d, exist_ok=True)
    for part in ("npm", "git"):
        source = os.path.join(fixtures, part)
        if os.path.isdir(source):
            copy_tree(source, os.path.join(agent, part))
    with open(SETTINGS_FIXTURE) as f:
        settings = json.load(f)
    settings["packages"] = packages
    with open(os.path.join(agent, "settings.json"), "w") as f:
        json.dump(settings, f)
    # Trust the sandbox cwd (and its parent) so no host prompts for it; TS Pi and pi-rs use the
    # same $PI_CODING_AGENT_DIR/trust.json.
    with open(os.path.join(agent, "trust.json"), "w") as f:
        json.dump({os.path.realpath(work): True, os.path.realpath(root): True}, f)
    return home, agent, work, tmp


def run_once(host, root, fixtures, packages, idle, cols, rows):
    home, agent, work, tmp = make_sandbox(root, fixtures, packages)
    env = {
        "HOME": home,
        "PI_CODING_AGENT_DIR": agent,
        "PI_OFFLINE": "1",
        "PI_TIMING": "1",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "TERM": "xterm-256color",
        "COLUMNS": str(cols),
        "LINES": str(rows),
        "LANG": os.environ.get("LANG", "C.UTF-8"),
        "TZ": "UTC",
        "TMPDIR": tmp,
    }
    sink = None
    if host["compiled_js"] == "node-hook":
        sink = os.path.join(tmp, "js")
        env["PI_JS_COMPILE_SINK"] = sink
        env["NODE_OPTIONS"] = f"--require {COUNTER}"

    started = time.monotonic()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(work)
        os.execvpe(host["argv"][0], host["argv"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    output = b""
    ready = None
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
            output += data
            seen = (seen + data)[-8192:]
            if READY_MARK in seen:
                ready = time.monotonic() - started
                break
        if ready is None:
            raise RuntimeError(
                f"{host['name']} did not reach the editor within {TIMEOUT:g}s"
            )
        # Keep draining while idle so the program never blocks on a full terminal.
        deadline = time.monotonic() + idle
        while time.monotonic() < deadline:
            if select.select([fd], [], [], 0.05)[0]:
                try:
                    output += os.read(fd, 65536)
                except OSError:
                    break
        pids = tree(pid)
        total, processes = footprint(pids)
    finally:
        for p in reversed(tree(pid)):
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        os.close(fd)

    text = strip_ansi(output.decode(errors="replace"))
    result = {
        "ready_ms": ready * 1000,
        "timings_ms": timing_total(text, "main"),
        "extensions_ms": timing_total(text, "extensions"),
        "idle_rss_kib": total["VmRSS"],
        "peak_rss_kib": total["VmHWM"],
        "threads": total["Threads"],
        "children": max(len(processes) - 1, 0),
        "processes": processes,
    }
    main = next((p for p in processes if p["pid"] == pid), {})
    result["main_idle_rss_kib"] = main.get("VmRSS", 0)
    result["main_peak_rss_kib"] = main.get("VmHWM", 0)
    result["children_idle_rss_kib"] = total["VmRSS"] - main.get("VmRSS", 0)
    result["children_peak_rss_kib"] = total["VmHWM"] - main.get("VmHWM", 0)

    if host["compiled_js"] == "timing-line":
        result["compiled_js_bytes"] = compiled_from_timings(text)
    else:
        result["compiled_js_bytes"] = sum_sinks(sink)
    return result


def strip_ansi(text):
    out, i = [], 0
    while i < len(text):
        c = text[i]
        if c == "\x1b":
            if text.startswith("\x1b]", i):
                end = text.find("\x07", i)
                i = len(text) if end < 0 else end + 1
                continue
            i += 1
            while i < len(text) and text[i] in "[]()#;?0123456789":
                i += 1
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


def timing_total(text, namespace):
    marker = f"--- Startup Timings: {namespace} ---"
    start = text.find(marker)
    if start < 0:
        return None
    total = text.find("TOTAL: ", start)
    if total < 0:
        return None
    value = text[total + len("TOTAL: ") :].split("ms", 1)[0]
    try:
        return float(value)
    except ValueError:
        return None


def compiled_from_timings(text):
    marker = "Compiled JS: "
    start = text.find(marker)
    if start < 0:
        return None
    value = text[start + len(marker) :].split(" bytes", 1)[0]
    try:
        return int(value)
    except ValueError:
        return None


def sum_sinks(sink):
    """Sum one counter file per node process (extensions spawn children that inherit the env)."""
    total = 0
    found = False
    for path in glob.glob(f"{sink}.*"):
        try:
            with open(path) as f:
                total += int(json.load(f).get("total", 0))
            found = True
        except (OSError, ValueError):
            continue
    return total if found else None


# ---------------------------------------------------------------- configurations


def load_configurations(fixtures):
    """(name, packages) for the empty set, each fixture package alone, and the full set."""
    with open(SETTINGS_FIXTURE) as f:
        all_packages = json.load(f)["packages"]
    configurations = [("none", [])]
    for package in all_packages:
        configurations.append((short_name(package), [package]))
    configurations.append(("all", all_packages))
    return configurations


def short_name(package):
    source = package["source"] if isinstance(package, dict) else package
    kind, _, rest = source.partition(":")
    if kind == "git":
        return rest.rstrip("/").split("/")[-1].removesuffix(".git")
    name = rest
    if name.startswith("@"):
        scope, _, name = name.rpartition("/")
        return f"{scope}/{name.split('@')[0]}"
    return name.split("@")[0]


# ---------------------------------------------------------------- statistics / output


def summarize(runs):
    metrics = {}
    for key in (
        "ready_ms",
        "timings_ms",
        "extensions_ms",
        "idle_rss_kib",
        "peak_rss_kib",
        "main_idle_rss_kib",
        "main_peak_rss_kib",
        "children_idle_rss_kib",
        "children_peak_rss_kib",
        "threads",
        "children",
        "compiled_js_bytes",
    ):
        values = [r[key] for r in runs if r.get(key) is not None]
        if not values:
            metrics[key] = {"median": None, "min": None, "max": None, "spread": None}
            continue
        median = statistics.median(values)
        metrics[key] = {
            "median": median,
            "min": min(values),
            "max": max(values),
            "spread": (max(values) - min(values)) / median * 100 if median else None,
        }
    return metrics


def format_value(key, metrics):
    entry = metrics.get(key, {})
    median = entry.get("median")
    if median is None:
        return "-"
    if key.endswith("_kib"):
        return f"{median / 1024:.1f} MiB"
    if key.endswith("_ms"):
        return f"{median:.0f} ms"
    if key == "compiled_js_bytes":
        return f"{median / 1024:.0f} KiB"
    return f"{median:.0f}"


def spread(metrics, key):
    value = metrics.get(key, {}).get("spread")
    return "-" if value is None else f"{value:.0f}%"


def print_table(results, hosts, configurations, runs):
    print(
        f"median of {runs} runs, 120x40 pty, idle sampled after ready; "
        "spread = (max-min)/median"
    )
    columns = ["ready", "timings", "extensions", "idle RSS", "peak RSS", "children RSS", "children", "compiled JS"]
    keys = ["ready_ms", "timings_ms", "extensions_ms", "idle_rss_kib", "peak_rss_kib", "children_peak_rss_kib", "children", "compiled_js_bytes"]
    for host in hosts:
        print(f"\n{host['name']}")
        print(f"{'config':<24}" + "".join(f"{c:>14}" for c in columns))
        for name, _ in configurations:
            metrics = results[host["name"]][name]
            row = f"{name:<24}"
            for key in keys:
                row += f"{format_value(key, metrics):>10} {spread(metrics, key):>3}"
            print(row)


def markdown(results, hosts, configurations, runs):
    lines = [
        f"Median of {runs} runs ({_machine()}), 120x40 pty, patches from the fixture cache, "
        "`PI_OFFLINE=1`.",
        "",
        "Spread is `(max - min) / median` over the runs. `children` counts spawned processes "
        "(bili proxy, context-mode server); their RSS is the tree total minus the host process.",
        "`compiled JS` is source bytes handed to the JS compiler before the editor is ready.",
        "",
    ]
    for host in hosts:
        lines.append(f"### {host['name']}")
        lines.append("")
        lines.append(
            "| configuration | ready | timings | extensions | idle RSS | peak RSS | children RSS | children | compiled JS |"
        )
        lines.append("|---|---|---|---|---|---|---|---|---|")
        for name, _ in configurations:
            metrics = results[host["name"]][name]
            cells = []
            for key in (
                "ready_ms",
                "timings_ms",
                "extensions_ms",
                "idle_rss_kib",
                "peak_rss_kib",
                "children_peak_rss_kib",
                "children",
                "compiled_js_bytes",
            ):
                cells.append(f"{format_value(key, metrics)} ±{spread(metrics, key)}")
            lines.append(f"| {name} | " + " | ".join(cells) + " |")
        lines.append("")
    return "\n".join(lines) + "\n"


def _machine():
    try:
        with open("/proc/cpuinfo") as f:
            model = next(
                (line.split(":", 1)[1].strip() for line in f if line.startswith("model name")),
                "unknown CPU",
            )
        cores = len(os.sched_getaffinity(0))
        return f"{model}, {cores} cores"
    except OSError:
        return "unknown machine"


# ---------------------------------------------------------------- budgets


def check_budgets(path, results, hosts, configurations):
    """Exit non-zero when a budgeted median is exceeded. No file means nothing to enforce."""
    if not os.path.exists(path):
        log(f"no {os.path.relpath(path, REPO)}; skipping budget checks")
        return []
    with open(path, "rb") as f:
        budgets = tomllib.load(f)
    violations = []
    for host in hosts:
        for name, _ in configurations:
            table = budgets.get(f"{host['name']}:{name}", {})
            metrics = results[host["name"]][name]
            for metric, budget in {**budgets.get("default", {}), **table}.items():
                median = metrics.get(metric, {}).get("median")
                if median is None:
                    continue
                if median > budget:
                    violations.append(
                        f"{host['name']}:{name} {metric} = {median:.0f} > budget {budget}"
                    )
    for line in violations:
        log(f"budget exceeded: {line}")
    return violations


# ---------------------------------------------------------------- self-test


def self_test(hosts, fixtures, idle, cols, rows):
    """Run the empty set once per host and assert the numbers are present and sane."""
    failures = []
    for host in hosts:
        with tempfile.TemporaryDirectory(prefix="pkp-selftest-") as root:
            result = run_once(host, root, fixtures, [], idle, cols, rows)
        log(f"self-test {host['name']}: {json.dumps({k: v for k, v in result.items() if k != 'processes'})}")
        if not 0 < result["ready_ms"] < 60_000:
            failures.append(f"{host['name']}: ready_ms {result['ready_ms']} out of range")
        if not result["peak_rss_kib"] > 1024:
            failures.append(f"{host['name']}: peak RSS {result['peak_rss_kib']} KiB too small")
        if result["children"] != 0:
            failures.append(f"{host['name']}: {result['children']} children with no packages")
        if result["timings_ms"] is None:
            failures.append(f"{host['name']}: no PI_TIMING main total")
        if result["compiled_js_bytes"] is None:
            failures.append(f"{host['name']}: compiled JS not reported")
    # Budget enforcement: a budget below the measured value must fail the run.
    fake = {host["name"]: {"none": summarize([{"ready_ms": 10.0, **{k: 1 for k in ("idle_rss_kib",)}}])} for host in hosts}
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as f:
        f.write("[default]\nready_ms = 1\n")
        budget_path = f.name
    try:
        if not check_budgets(budget_path, fake, hosts, [("none", [])]):
            failures.append("budget check did not flag a median above its budget")
    finally:
        os.unlink(budget_path)

    for failure in failures:
        log(f"self-test FAILED: {failure}")
    if not failures:
        log("self-test passed: empty set measured on every host, budget check trips")
    return 0 if not failures else 1


# ---------------------------------------------------------------- main


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", nargs="?", default=os.path.join(REPO, "target/release/pi"), help="pi-rs binary (default: target/release/pi)")
    parser.add_argument("--host", action="append", choices=["pi-rs", "pi-ts"], help="host(s) to measure (default: both when TS Pi is configured, else pi-rs)")
    parser.add_argument("--ts", help='TS Pi command, e.g. "node /path/to/packages/coding-agent/dist/bundle/cli.js"')
    parser.add_argument("--config", action="append", help="configuration name(s) to run (default: all)")
    parser.add_argument("--runs", type=int, default=int(os.environ.get("RUNS", "5")))
    parser.add_argument("--idle", type=float, default=float(os.environ.get("IDLE_SECONDS", "1")))
    parser.add_argument("--columns", type=int, default=int(os.environ.get("COLUMNS", "120")))
    parser.add_argument("--lines", type=int, default=int(os.environ.get("LINES", "40")))
    parser.add_argument("--fixtures", default=os.environ.get("PI_RS_PKG_FIXTURES_DIR", os.path.join(REPO, "target/pkg-fixtures")))
    parser.add_argument("--json", help="write raw results as JSON")
    parser.add_argument("--markdown", help="write the baseline table as markdown")
    parser.add_argument("--budgets", default=os.path.join(REPO, "perf/budgets.toml"))
    parser.add_argument("--self-test", action="store_true", help="empty set, one run, assert sane numbers")
    args = parser.parse_args()

    fixtures = os.path.join(args.fixtures, "agent")
    if not os.path.isdir(os.path.join(fixtures, "npm")):
        parser.error(
            f"no fixture packages at {fixtures}; run scripts/package-fixtures.sh first"
        )

    ts = args.ts or os.environ.get("PI_PERF_TS")
    hosts = []
    wanted = args.host or (["pi-rs", "pi-ts"] if ts else ["pi-rs"])
    if "pi-rs" in wanted:
        if not os.access(args.binary, os.X_OK):
            parser.error(f"no executable at {args.binary}; build it with: cargo build --release --locked -p pi-coding-agent --bin pi")
        hosts.append({"name": "pi-rs", "argv": [args.binary], "compiled_js": "timing-line"})
    if "pi-ts" in wanted:
        if not ts:
            parser.error("--host pi-ts needs --ts or PI_PERF_TS")
        hosts.append({"name": "pi-ts", "argv": ts.split(), "compiled_js": "node-hook"})

    if args.self_test:
        return self_test(hosts, fixtures, args.idle, args.columns, args.lines)

    configurations = load_configurations(fixtures)
    if args.config:
        known = dict(configurations)
        missing = [name for name in args.config if name not in known]
        if missing:
            parser.error(f"unknown configuration(s) {missing}; known: {list(known)}")
        configurations = [(name, known[name]) for name in args.config]

    results = {}
    scratch = tempfile.mkdtemp(prefix="pkp-footprint-")
    try:
        for host in hosts:
            # Warm-up: page cache and lazy loading of the host's own runtime.
            with tempfile.TemporaryDirectory(prefix="warm-", dir=scratch) as root:
                run_once(host, root, fixtures, [], args.idle, args.columns, args.lines)
            results[host["name"]] = {}
            for name, packages in configurations:
                runs = []
                for index in range(args.runs):
                    with tempfile.TemporaryDirectory(prefix="run-", dir=scratch) as root:
                        result = run_once(host, root, fixtures, packages, args.idle, args.columns, args.lines)
                    runs.append(result)
                    log(f"{host['name']}:{name} run {index + 1}/{args.runs} ready {result['ready_ms']:.0f}ms")
                results[host["name"]][name] = summarize(runs)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    print_table(results, hosts, configurations, args.runs)
    if args.markdown:
        with open(args.markdown, "w") as f:
            f.write(markdown(results, hosts, configurations, args.runs))
        log(f"wrote {args.markdown}")
    if args.json:
        with open(args.json, "w") as f:
            json.dump({"runs": args.runs, "hosts": [h["name"] for h in hosts], "results": results}, f, indent=2)
        log(f"wrote {args.json}")

    violations = check_budgets(args.budgets, results, hosts, configurations)
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
