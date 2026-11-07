# Package footprint baseline (PKGP-02)

What unchanged upstream Pi packages cost pi-rs and TS Pi 1.4.0 at startup and at rest, in four
dimensions: time to first prompt-ready, RSS of the host and of the child processes it spawns,
and JavaScript source bytes handed to the JS compiler.

Measured with `scripts/perf/packages.py` over the PKGP-01 fixture set
(`crates/pi-coding-agent/tests/fixtures/packages/`), 5 runs per cell, median and
`(max - min) / median` spread.

## Method

```text
scripts/package-fixtures.sh                                     # once, the only network step
cargo build --release --locked -p pi-coding-agent --bin pi
PI_PERF_TS="node <ref>/pi/packages/coding-agent/dist/bundle/cli.js" \
  scripts/perf/packages.py --runs 5 --json out.json --markdown docs/perf/packages.md
```

Each run gets a fresh `HOME` and `PI_CODING_AGENT_DIR` holding the pinned fixture packages
(`cp -a --reflink=auto` from `target/pkg-fixtures/agent`), `PI_OFFLINE=1`, `PI_TIMING=1`, and a
`trust.json` naming the sandbox so neither host prompts for trust. The host runs in a 120x40 pty;
`ready` stops at the first frame that contains the footer's `(auto)` gauge, then the script waits
`--idle` seconds, sums `/proc/<pid>/status` over the whole process tree, and samples children
separately. Nothing reads the user's own setup and no network is used.

| column | meaning |
|---|---|
| `ready` | spawn to first prompt-ready editor frame |
| `timings` | `PI_TIMING=1` main total (`crates/pi-coding-agent/src/core/timings.rs`, TS `core/timings.ts`) |
| `extensions` | the `PI_TIMING` extensions total: provider/module import plus factory call |
| `idle RSS` | summed `VmRSS` of the tree `--idle` (1 s) after ready |
| `peak RSS` | summed `VmHWM` of the tree at the same moment |
| `children RSS` | tree total minus the host process — the spawned servers |
| `children` | number of descendant processes (bili proxy, context-mode server, …) |
| `compiled JS` | source bytes compiled before ready (see below) |

`compiled JS` is counted per host, because the two hosts compile differently:

- pi-rs: QuickJS reports it. `crates/pi-quickjs/src/lib.rs` adds `code.len()` in
  `Context::eval_with_flags` (the single `JS_Eval2` call site) to a process-wide counter, and
  `print_timings()` prints `Compiled JS: N bytes`.
- TS Pi: Node cannot report it, so the host runs with
  `NODE_OPTIONS="--require scripts/perf/js-compile-counter.cjs"`. The preload wraps CommonJS
  `Module._compile`, the ESM `module.registerHooks` chain (jiti replaces the chain rather than
  chaining onto it, so `registerHooks` itself is wrapped) and the `vm` compilers jiti/Babel fall
  back to. One sink file per process **and thread** (`$PI_JS_COMPILE_SINK.<pid>.<thread>`),
  written synchronously because the harness stops hosts with `SIGKILL`.

## Baseline

Median of 5 runs, Intel i7-10700K (16 cores), 2026-11-07, release build of pi-rs at
`8101826` plus this PR; TS Pi 1.4.0, node v26.13.0. The run that produced this table wrote its
raw per-run data with `--json`; rerun the command above to regenerate the same shape.

### pi-rs

| configuration | ready | timings | extensions | idle RSS | peak RSS | children RSS | children | compiled JS |
|---|---|---|---|---|---|---|---|---|
| none | 101 ms ±25% | 81 ms ±31% | 16 ms ±50% | 55.0 MiB ±3% | 55.0 MiB ±10% | 0.0 MiB | 0 | 0 KiB |
| ponytail | 132 ms ±37% | 112 ms ±43% | 54 ms ±31% | 65.8 MiB ±1% | 65.8 MiB ±3% | 0.0 MiB | 0 | 474 KiB |
| pi-mcp-adapter | 280 ms ±30% | 259 ms ±32% | 189 ms ±28% | 93.2 MiB ±2% | 93.7 MiB ±2% | 0.0 MiB | 0 | 2382 KiB |
| pi-subagents | 343 ms ±4% | 323 ms ±4% | 265 ms ±3% | 80.6 MiB ±1% | 81.3 MiB ±1% | 0.0 MiB | 0 | 3995 KiB |
| billion-context | 699 ms ±18% | 679 ms ±18% | 624 ms ±16% | 284.5 MiB ±4% | 290.1 MiB ±4% | 181.4 MiB ±2% | 1 | 6734 KiB |
| pi-web-access | 248 ms ±35% | 229 ms ±38% | 160 ms ±38% | 72.5 MiB ±2% | 72.5 MiB ±2% | 0.0 MiB | 0 | 2054 KiB |
| @juicesharp/rpiv-ask-user-question | 215 ms ±37% | 195 ms ±42% | 115 ms ±39% | 69.3 MiB ±1% | 69.3 MiB ±1% | 0.0 MiB | 0 | 895 KiB |
| @juicesharp/rpiv-todo | 225 ms ±24% | 203 ms ±24% | 117 ms ±30% | 68.9 MiB ±1% | 68.9 MiB ±1% | 0.0 MiB | 0 | 880 KiB |
| context-mode | 215 ms ±87% | 194 ms ±95% | 100 ms ±85% | 68.4 MiB ±2% | 68.4 MiB ±2% | 0.0 MiB | 0 | 967 KiB |
| all | 1829 ms ±24% | 1806 ms ±25% | 1709 ms ±26% | 326.1 MiB ±4% | 335.4 MiB ±6% | 181.0 MiB ±8% | 1 | 13263 KiB |

### TS Pi 1.4.0

| configuration | ready | timings | extensions | idle RSS | peak RSS | children RSS | children | compiled JS |
|---|---|---|---|---|---|---|---|---|
| none | 685 ms ±45% | 79 ms ±97% | 6 ms ±183% | 130.1 MiB ±3% | 130.1 MiB ±3% | 0.0 MiB | 0 | 5425 KiB |
| ponytail | 696 ms ±18% | 134 ms ±32% | 51 ms ±20% | 136.4 MiB ±2% | 136.4 MiB ±2% | 0.0 MiB | 0 | 5843 KiB |
| pi-mcp-adapter | 2273 ms ±44% | 1857 ms ±46% | 1799 ms ±47% | 282.8 MiB ±9% | 282.8 MiB ±9% | 0.0 MiB | 0 | 11277 KiB ±22% |
| pi-subagents | 3168 ms ±13% | 2693 ms ±16% | 2618 ms ±17% | 407.4 MiB ±1% | 407.4 MiB ±1% | 0.0 MiB | 0 | 14416 KiB |
| billion-context | 755 ms ±25% | 367 ms ±22% | 310 ms ±21% | 374.7 MiB ±1% | 374.7 MiB ±1% | 182.1 MiB ±1% | 1 | 17437 KiB |
| pi-web-access | 3189 ms ±15% | 2698 ms ±15% | 2637 ms ±15% | 358.5 MiB ±2% | 358.5 MiB ±2% | 0.0 MiB | 0 | 11032 KiB |
| @juicesharp/rpiv-ask-user-question | 1014 ms ±21% | 547 ms ±31% | 486 ms ±32% | 198.8 MiB ±7% | 198.8 MiB ±7% | 0.0 MiB | 0 | 8904 KiB |
| @juicesharp/rpiv-todo | 878 ms ±17% | 488 ms ±23% | 430 ms ±22% | 193.1 MiB ±8% | 193.1 MiB ±8% | 0.0 MiB | 0 | 8886 KiB |
| context-mode | 485 ms ±23% | 105 ms ±30% | 46 ms ±48% | 139.5 MiB ±1% | 139.5 MiB ±1% | 0.0 MiB | 0 | 6218 KiB |
| all | 7253 ms ±44% | 6607 ms ±48% | 6418 ms ±49% | 725.6 MiB ±31% | 740.2 MiB ±31% | 180.8 MiB ±2% | 1 | 33323 KiB ±8% |

### What the numbers say

- Per-extension cost tracks the package's bundle size, not the host. `billion-context` alone
  compiles 6734 KiB on pi-rs against 17437 KiB on TS Pi and reaches ready in 699 ms against
  755 ms; `pi-subagents` (3995 KiB vs 14416 KiB) costs 343 ms vs 3168 ms.
- `billion-context` is the only fixture that spawns a process at startup, on both hosts: the
  `bili` proxy (`node <agentDir>/npm/node_modules/billion-context/dist/index.js start --host …
  --port …`), about 181 MiB of tree RSS. Its `dist/index.js` is a single 5.9 MB ESM file, and
  starting it costs Node itself; that child is most of the 343→1829 ms and 81→326 MiB gap
  between the next-priciest package alone and the full set.
- `context-mode` spawns no process at startup on either host (967 KiB compiled, 0 children),
  so its cost here is its module import only.
- Whole-tree peak RSS for the full set: pi-rs 335 MiB, TS Pi 740 MiB. The idle tree is the same
  order of magnitude per host (pi-rs 326 MiB, TS Pi 726 MiB).

## Budgets

`scripts/perf/packages.py` reads `perf/budgets.toml` and **exits 1** when any median exceeds its
budget:

```toml
[default]
ready_ms = 4000

["pi-rs:all"]
idle_rss_kib = 700000
children = 1
```

Keys are metric names (`ready_ms`, `timings_ms`, `extensions_ms`, `idle_rss_kib`,
`peak_rss_kib`, `children_idle_rss_kib`, `children_peak_rss_kib`, `threads`, `children`,
`compiled_js_bytes`); `[default]` applies to every cell, `["<host>:<configuration>"]` overrides
per cell, and a cell with no budget is not checked. Missing file → no checks, exit 0.

The committed file holds this baseline's medians with roughly 2x headroom (marked
`PROVISIONAL`), so the check is wired and fails on regressions now; PKGP-22 replaces the numbers.

## Self-test

```text
scripts/perf/packages.py --self-test
```

Runs the empty set once per selected host and fails when ready time is out of range or absent,
peak RSS is implausibly small, a child process exists with no packages, the `PI_TIMING` main
total is missing, or the compiled-JS figure is not reported; it also runs the budget check against
a synthetic result and fails when an exceeded budget is not flagged. Currently passes for pi-rs
(ready ~101-138 ms, ~55 MiB, 0 children, a `PI_TIMING` main total, compiled bytes reported).

## Known limits

- RSS comes from `VmHWM`/`VmRSS`, so a child that exits before sampling is not counted, and
  shared pages are counted once per process in the tree total.
- `compiled JS` counts source bytes (QuickJS `code.len()`; Node's `source` string or buffer),
  not compiler work or bytecode.
- TS Pi children inherit `NODE_OPTIONS`, so their compiled bytes are counted too; pi-rs children
  are separate runtimes (Node for the bili proxy) and their JS is not counted by the QuickJS
  counter. Compare `compiled JS` as a per-host figure, not across hosts.
- Spreads above ±30% are machine noise under parallel builds (this baseline ran while other
  work was building); `ready`/`extensions` spreads shrink on an idle machine, values do not move
  by more than the spread.
