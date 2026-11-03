# Porting pi to Rust

This repository is a 1:1 Rust port of the pi coding agent
(<https://github.com/earendil-works/pi>, `packages/coding-agent` and the packages it
depends on). The goal is feature parity: same CLI, same TUI look and behavior, same
on-disk formats (sessions, settings, auth, models.json), same RPC protocol, same
extension (plugin) capabilities.

## Reference source

The TypeScript reference is cloned at:

    <any checkout of https://github.com/earendil-works/pi at the tag you port against, e.g. `git clone https://github.com/earendil-works/pi.git && git -C pi checkout v1.4.0`>

The golden/differential tests read it from `PI_TS_REF` (generators in `scripts/golden/`, `scripts/golden/run.sh`) or `PI_TS_SOURCE` (example-extension tests); without it they skip (see INFRA-04).
Third-party Pi packages are checked by the package smoke harness (`crates/pi-coding-agent/tests/packages/`): `scripts/package-fixtures.sh` fetches the pinned packages once, `PI_RS_PKG_FIXTURES=1 cargo test -p pi-coding-agent --test packages` runs them offline, and `scripts/package-golden.sh` re-records the TS Pi 1.4.0 golden.
Never copy TypeScript into this repository.

## Crate layout

| TS package                      | Rust crate                  | Notes |
|---------------------------------|-----------------------------|-------|
| `packages/chord` durable subset  | `crates/pi-chord`            | JSON revisions, context, watches, output and SQLite contracts |
| `packages/durable`               | `crates/pi-durable`          | In progress; pinned scope and parity ledger in `porting/durable/` |
| `packages/ai`                   | `crates/pi-ai`              | Providers, model catalog, streaming, OAuth |
| `packages/telemetry`            | `crates/pi-telemetry`       | |
| `packages/agent`                | `crates/pi-agent`           | Agent loop |
| `packages/tui`                  | `crates/pi-tui`             | Terminal UI framework and components |
| `packages/mcp`                  | `crates/pi-mcp`             | MCP client, transports, OAuth |
| `packages/codemode`             | `crates/pi-codemode`        | Code mode runtime (JS scripts calling tools) |
| (new)                           | `crates/pi-quickjs`         | Embedding API over QuickJS-ng (rquickjs): refcounted values, objects, functions, errors bound to the engine thread. |
| (new)                           | `crates/pi-jsrt`            | Embedded JavaScript runtime (QuickJS via `pi-quickjs` + oxc TS stripping, ESM loader, Node API shims, Node mode / `pi-jsrt-node`). Used by extensions and codemode. |
| (new)                           | `crates/pi-json`            | V8-compatible `JSON.parse` messages and JS `JSON.stringify` formatting shared by the crates. |
| (new)                           | `crates/pi-embed`, `crates/pi-embed-macros` | Compressed embedded text assets (`Embedded::get()`), used for the JS glue and docs. |
| (new)                           | `crates/pi-durable-jsrt`    | Serves `@earendil-works/pi-durable` and `@earendil-works/chord` to extensions as embedded pi-jsrt modules. |
| (new)                           | `crates/pi-durable-codec-stock` | Test-only WebAssembly bindings that compile the production durable codec sources for the unchanged stock codec tests. |
| `packages/coding-agent`         | `crates/pi-coding-agent`    | The `pi` binary (`src/main.rs`) and library |

Module layout mirrors the TS file layout: `src/core/session-manager.ts` becomes
`src/core/session_manager.rs`, directories become modules.

## Conventions

- Rust 2024 edition, async on tokio. Shared dependency versions live in the root
  `Cargo.toml` `[workspace.dependencies]`; use `dep.workspace = true`.
- Pure Rust: no Node/Bun at runtime. HTTP via `reqwest` (rustls).
- Wire and disk formats must be byte-compatible with the TS version: use serde with
  `#[serde(rename_all = "camelCase")]`, tagged enums matching the TS discriminants
  (`#[serde(tag = "type")]` / `tag = "role"`), and `skip_serializing_if` for optional
  fields so JSON output matches.
- Behavior is ported faithfully, including edge cases, error messages, defaults and
  environment variables. Read the TS file in full before porting it.
- Port unit tests that do not need network access or real API keys into Rust
  `#[cfg(test)]` modules or `tests/` and make them pass.
- No `todo!()`/`unimplemented!()` placeholders in finished work. If something cannot
  be ported yet, record it in `PORTING-STATUS.md` with the reason.
- `cargo check -p <crate>` / `cargo test -p <crate>` for the crate you work on. Keep the
  crate free of warnings (`cargo clippy -p <crate>` clean is a bonus).
- Commit only paths you changed: `git add <paths>` then
  `git commit -m "feat(<crate>): ..."`. Never `git add -A`, `git reset --hard`,
  `git stash`, `git checkout .`. If `.git/index.lock` exists, wait a few seconds and retry.
  Do not push; the coordinator pushes.

## Extensions in Rust

pi extensions are TypeScript modules. The Rust port keeps that contract:

- The extension API (events, tools, commands, shortcuts, flags, providers, renderers,
  UI context, session control) is defined as Rust types in
  `pi-coding-agent::core::extensions`. Built-in extensions (`src/extensions/*`: mcp,
  codemode, tool-search, llama) are Rust implementations against that API.
- User extensions (`.ts`/`.js`) run inside `pi-jsrt`, an embedded QuickJS engine on a
  dedicated thread. TypeScript is stripped with oxc. Imports of
  `@earendil-works/pi-coding-agent`, `@earendil-works/pi-tui`, `@earendil-works/pi-ai`,
  `@earendil-works/pi-agent-core`, `typebox` and `node:*` builtins resolve to modules
  provided by the runtime, so existing extensions load unchanged.
