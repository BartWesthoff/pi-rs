# Local patches to rquickjs-sys 0.14.0 (QuickJS-ng)

pi-jsrt runs JavaScript on QuickJS-ng through rquickjs. These changes to `quickjs/quickjs.c` and
`quickjs/quickjs.h` add the host hooks pi-jsrt needs and fix two spec deviations. Unused files of
the upstream crate (QuickJS tests, docs, the REPL and libc) are removed; `build.rs` additionally
reruns when the QuickJS sources change. The new functions are not in the pre-generated bindings:
pi-jsrt declares them itself (`crates/pi-jsrt/src/js/mod.rs`, module `ext`).

## Promise job context hooks (`JS_SetJobContextHooks`)

QuickJS-ng's promise hooks only wrap thenable jobs, so a host cannot carry state (Node's
`AsyncLocalStorage`, `async_hooks` ids) from where a reaction is created to where it runs.
`JS_SetJobContextHooks(rt, capture, enter, exit, opaque)` adds the equivalent of the spec's
HostMakeJobCallback / HostCallJobCallback (V8's continuation-preserved embedder data):

- `capture` runs when `perform_promise_then` creates the reactions of a `then` (and of every
  `await`) and when a thenable job is queued; its result is stored in the reaction
  (`JSPromiseReactionData.job_context`, marked for the cycle collector) or passed as the job's
  fourth argument.
- `enter` / `exit` run around the handler call of `promise_reaction_job` and the `then` call of
  `js_promise_resolve_thenable_job`.

Regression coverage: `crates/pi-jsrt/tests/async_context.rs`.

## Local timezone hook (`JS_SetTimezoneOffsetFunc`)

`Date` read the local timezone from the C library (`localtime_r`, i.e. the process's `TZ`).
pi-jsrt gives every runtime its own timezone (`process.env.TZ`, as in Node), so
`getTimezoneOffset` now asks the runtime's hook first. `set_date_fields` takes the context to
reach it.

Regression coverage: `crates/pi-jsrt/tests/process.rs::process_env_tz_sets_the_runtime_timezone`.

## Clock hook (`JS_SetDateNowFunc`)

`Date.now()` and `new Date()` read the system clock. Code mode's deterministic VM gives scripts a
virtual clock (the time of the result being processed), so `date_now` asks the runtime's hook
first.

Regression coverage: `crates/pi-codemode/tests/sandbox.rs` (deterministic replays).

## Replaceable global object (`JS_SetGlobalObject`)

`node:vm` contextified objects need a realm whose global object is a Proxy (see
`crates/pi-jsrt/src/ops/vm.rs`). `JS_SetGlobalObject(ctx, obj)` replaces `ctx->global_obj`. Global
`var` and function declarations and `delete` of global names went straight to the global
object's shape (`find_own_property1`, `add_property`, `delete_property`); when the global object
is a Proxy they now use its internal methods (`[[GetOwnProperty]]`, `[[DefineOwnProperty]]`,
`[[Delete]]`), as the spec's CreateGlobalVarBinding / CreateGlobalFunctionBinding do.

Regression coverage: `crates/pi-jsrt/tests/vm.rs`.

## Proxy `[[DefineOwnProperty]]` invariant with `writable: true`

The invariant check after a `defineProperty` trap rejected any descriptor with a `writable` field
(and any value-only descriptor) for a non-configurable, writable target property. The spec
(ProxyCreate [[DefineOwnProperty]] step 16.c) rejects only an explicit `writable: false`.
Global `var` declarations through a vm Proxy global hit this.

Regression coverage: `crates/pi-jsrt/tests/vm.rs`.

## `"NaN"` is a canonical numeric string

`CanonicalNumericIndexString` (`JS_AtomIsNumericIndex1`) skipped `"NaN"`, so `typedArray["NaN"] = 1`
created an own property instead of being ignored like the other non-integer numeric keys.

Regression coverage:
`crates/pi-jsrt/tests/engine_regressions.rs::typed_array_canonical_numeric_keys_are_still_integer_indexed`.

## Synchronous `require()` of ES modules (`JS_RequireModule`)

`JS_LoadModule` settles its promise through a promise job, so pi-jsrt's `require()` of an ES
module ran the whole job queue to get the namespace. When that `require()` ran while a module
graph with top-level await was still being evaluated (an extension that imports a TLA module and
calls pi-ai's `StringEnum`, which lazily requires TypeBox), the queued jobs resumed the async
module and `gather_available_ancestors` walked parents that were still on the synchronous
evaluation stack: a segmentation fault. Draining the queue also ran unrelated microtasks in the
middle of a synchronous call.

`JS_RequireModule(ctx, basename, filename, &status)` resolves, links and evaluates the module
without running jobs and returns its namespace, as Node's `require(esm)` does. It reports a graph
with top-level await (`JS_REQUIRE_MODULE_ASYNC`, checked before anything is evaluated) or one that
contains a module being linked or evaluated (`JS_REQUIRE_MODULE_CYCLE`) instead of evaluating it;
pi-jsrt turns these into Node's `ERR_REQUIRE_ASYNC_MODULE` and `ERR_REQUIRE_CYCLE_MODULE`. An
evaluation error is thrown (the module promise is marked handled, so it is not also reported as
an unhandled rejection). A module that has already finished evaluating returns its namespace.

Regression coverage: `crates/pi-jsrt/tests/require_esm.rs`.

## Modules evaluated after an async dependency are done

When a top-level-await module settled, `js_async_module_execution_fulfilled` ran its synchronous
ancestors and marked them evaluated but left their `async_evaluation` flag set. A later `import()`
of any module that depends on one of them then registered itself as waiting on that module, which
never settles again, so the import never resolved (an extension's lazy `import()` of a module
that shares dependencies with a top-level-await module hung forever). As in ECMA-262
(AsyncModuleExecutionFulfilled sets `[[AsyncEvaluationOrder]]` to `done` for those modules), the
flag is now cleared before the module is marked evaluated.

Regression coverage:
`crates/pi-jsrt/tests/engine_regressions.rs::import_after_async_graph_settles_does_not_wait_on_evaluated_modules`.

## Interrupting `Atomics.wait` (`JS_InterruptAtomicsWait`)

pi-jsrt lets every runtime block in `Atomics.wait` (`JS_SetCanBlock`, as Node does on the main
thread too), but a thread blocked without a timeout never returned to its event loop, so
`worker.terminate()` or a runtime shutdown could not stop it. `JS_InterruptAtomicsWait(rt)` (callable
from any thread while `rt` is alive) marks the runtime under the Atomics mutex and signals its
waiters (each waiter now records its runtime); the woken `Atomics.wait`, and every later one in that
runtime, throws the uncatchable "interrupted" error of the interrupt handler, so the stack unwinds
to the event loop, which then sees the shutdown.

Regression coverage: `crates/pi-jsrt/tests/atomics.rs`
(`terminate_wakes_a_blocked_worker_and_a_crashed_worker_times_out`,
`embedded_runtimes_block_in_wait_and_shutdown_wakes_them`).
