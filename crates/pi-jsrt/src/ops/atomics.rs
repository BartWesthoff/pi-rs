//! `Atomics.waitAsync` and `SharedArrayBuffer` aliases.
//!
//! QuickJS implements `Atomics.wait`/`notify` with a process-wide waiter list, which works across
//! runtimes because every `SharedArrayBuffer` of every runtime is backed by the shared allocator
//! of `pi-quickjs` ([`SharedMemory`]). `Atomics.waitAsync` has no QuickJS counterpart: its waiters
//! are kept here, keyed by the address they wait on, and `Atomics.notify` (wrapped in
//! `js/web/globals.js`) wakes them after QuickJS's synchronous waiters. A woken waiter's runtime
//! settles its promise through `internal.atomicsWake(id, result)`.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::engine::{self, OpFn, Poster, arg_f64, js_str, state};
use crate::js::{Context, JsArgs, JsNativeError, JsResult, JsTypedArray, JsValue, SharedMemory, TypedArrayKind};

struct AsyncWaiter {
    id: u64,
    /// The address of the awaited element.
    addr: usize,
    /// Keeps the memory (and so `addr`) from being reused while the waiter is pending.
    _memory: SharedMemory,
    poster: Poster,
    runtime: usize,
}

/// Pending async waiters across all runtimes, in the order they started waiting.
static WAITERS: LazyLock<Mutex<Vec<AsyncWaiter>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static NEXT_WAITER: AtomicU64 = AtomicU64::new(1);

fn runtime_key(ctx: &Context) -> usize {
    std::sync::Arc::as_ptr(&state(ctx).shared) as usize
}

/// The element `index` of the shared integer typed array `ta`: its memory, address and kind.
/// The glue has validated the array, its sharedness and the index.
fn element(ta: &JsValue, index: usize, ctx: &mut Context) -> JsResult<(SharedMemory, usize, TypedArrayKind)> {
    let obj = ta.as_object().ok_or_else(|| engine::type_error("expected a typed array"))?;
    let ta = JsTypedArray::from_object(obj)?;
    let kind = ta.kind().ok_or_else(|| engine::type_error("expected a typed array"))?;
    let buffer = ta.buffer(ctx)?;
    let memory = SharedMemory::from_value(&buffer, ctx)?.ok_or_else(|| engine::type_error("not a shared typed array"))?;
    let offset = ta.byte_offset(ctx)? + index * kind.element_size();
    if offset + kind.element_size() > memory.len() {
        return Err(JsNativeError::range().with_message("Invalid atomic access index").into());
    }
    let addr = memory.as_ptr() as usize + offset;
    Ok((memory, addr, kind))
}

/// Whether the element at `addr` holds `expected` (sequentially consistent load).
fn holds(addr: usize, kind: TypedArrayKind, expected: &JsValue, ctx: &mut Context) -> JsResult<bool> {
    Ok(match kind {
        TypedArrayKind::BigInt64 => {
            let want = expected.to_big_int64(ctx)?;
            // SAFETY: `addr` is an aligned element of live shared memory (typed array elements are
            // aligned to their size and blocks to 16 bytes); other threads only access it
            // through atomics or racy JS reads/writes, as with any SharedArrayBuffer.
            unsafe { AtomicI64::from_ptr(addr as *mut i64) }.load(Ordering::SeqCst) == want
        }
        _ => {
            let want = expected.to_i32(ctx)?;
            // SAFETY: as above, for a 4-byte element.
            unsafe { AtomicI32::from_ptr(addr as *mut i32) }.load(Ordering::SeqCst) == want
        }
    })
}

/// `ops.atomicsWaitAsync(ta, index, value, immediate) -> "not-equal" | "timed-out" | id`: register
/// an async waiter on `ta[index]` unless it no longer holds `value` (or `immediate`, a zero
/// timeout, asks only for that check).
fn op_atomics_wait_async(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let index = arg_f64(args, 1, ctx)? as usize;
    let (memory, addr, kind) = element(args.get_or_undefined(0), index, ctx)?;
    let value = args.get_or_undefined(2).clone();
    let immediate = args.get_or_undefined(3).to_boolean();
    // The check and the registration are atomic with respect to `atomicsNotify`, which takes the
    // same lock after the notifying thread's store.
    let mut waiters = WAITERS.lock();
    if !holds(addr, kind, &value, ctx)? {
        return Ok(js_str("not-equal"));
    }
    if immediate {
        return Ok(js_str("timed-out"));
    }
    let id = NEXT_WAITER.fetch_add(1, Ordering::Relaxed);
    waiters.push(AsyncWaiter { id, addr, _memory: memory, poster: state(ctx).poster(), runtime: runtime_key(ctx) });
    Ok(JsValue::from(id as f64))
}

/// `ops.atomicsCancelWait(id) -> bool`: the waiter timed out; false when it was already woken.
fn op_atomics_cancel_wait(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let mut waiters = WAITERS.lock();
    let found = waiters.iter().position(|w| w.id == id).map(|i| waiters.remove(i));
    Ok(JsValue::from(found.is_some()))
}

/// `ops.atomicsNotify(ta, index, count) -> number`: wake up to `count` async waiters on
/// `ta[index]`, oldest first.
fn op_atomics_notify(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let index = arg_f64(args, 1, ctx)? as usize;
    let count = arg_f64(args, 2, ctx)?;
    let (_memory, addr, _) = element(args.get_or_undefined(0), index, ctx)?;
    let mut woken = Vec::new();
    {
        let mut waiters = WAITERS.lock();
        let mut i = 0;
        while i < waiters.len() && (woken.len() as f64) < count {
            if waiters[i].addr == addr {
                woken.push(waiters.remove(i));
            } else {
                i += 1;
            }
        }
    }
    let n = woken.len();
    for w in woken {
        let id = w.id;
        w.poster.post(move |ctx| super::worker::dispatch(ctx, "atomicsWake", vec![JsValue::from(id as f64), js_str("ok")]));
    }
    Ok(JsValue::from(n as f64))
}

/// `ops.sabAlias(sab) -> SharedArrayBuffer`: a new buffer object over the same memory (what
/// `structuredClone` gives for a `SharedArrayBuffer`).
fn op_sab_alias(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let memory = SharedMemory::from_value(args.get_or_undefined(0), ctx)?.ok_or_else(|| engine::type_error("expected a SharedArrayBuffer"))?;
    memory.to_js(ctx)
}

/// Drop the async waiters of a runtime that is going away.
pub(crate) fn teardown(ctx: &mut Context) {
    let me = runtime_key(ctx);
    WAITERS.lock().retain(|w| w.runtime != me);
}

pub(crate) const OPS: &[(&str, usize, OpFn)] = &[
    ("atomicsWaitAsync", 4, op_atomics_wait_async),
    ("atomicsCancelWait", 1, op_atomics_cancel_wait),
    ("atomicsNotify", 3, op_atomics_notify),
    ("sabAlias", 1, op_sab_alias),
];
