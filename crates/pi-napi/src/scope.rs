//! Handle scopes, scripts, async work and the remaining lifecycle entry points.

use std::ffi::{c_char, c_void};

use crate::env::{self, guard, guard_js};
use crate::sys::{self, napi_callback_scope, napi_env, napi_status, napi_value};

/// `napi_open_handle_scope`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_open_handle_scope(env: napi_env, result: *mut sys::napi_handle_scope) -> napi_status {
    guard(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let s = e.open_scope(false);
        unsafe { *result = s };
        sys::napi_ok
    })
}

/// `napi_close_handle_scope`
///
/// # Safety
/// `scope` must come from `napi_open_handle_scope`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_close_handle_scope(env: napi_env, scope: sys::napi_handle_scope) -> napi_status {
    guard(env, |e| e.close_scope(scope))
}

/// `napi_open_escapable_handle_scope`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_open_escapable_handle_scope(env: napi_env, result: *mut sys::napi_escapable_handle_scope) -> napi_status {
    guard(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let s = e.open_scope(true);
        unsafe { *result = s };
        sys::napi_ok
    })
}

/// `napi_close_escapable_handle_scope`
///
/// # Safety
/// `scope` must come from `napi_open_escapable_handle_scope`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_close_escapable_handle_scope(env: napi_env, scope: sys::napi_escapable_handle_scope) -> napi_status {
    guard(env, |e| e.close_scope(scope))
}

/// `napi_escape_handle`
///
/// # Safety
/// `escaped` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_escape_handle(
    env: napi_env,
    scope: sys::napi_escapable_handle_scope,
    escapee: napi_value,
    escaped: *mut napi_value,
) -> napi_status {
    guard(env, |e| {
        if escaped.is_null() {
            return sys::napi_invalid_arg;
        }
        let status = e.escape(scope, escapee);
        if status == sys::napi_ok {
            unsafe { *escaped = escapee };
        }
        status
    })
}

/// `napi_run_script`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_run_script(env: napi_env, script: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { env::value(script) }) else { return sys::napi_invalid_arg };
        let Some(code) = v.as_string() else { return sys::napi_string_expected };
        let mut ctx = e.ctx.clone();
        let src = code.to_std_string_lossy();
        match ctx.eval(pi_quickjs::Source::from_bytes(src.as_bytes())) {
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_fatal_error`
///
/// # Safety
/// `location`, `message` are C strings (or `location_len`/`message_len` bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_fatal_error(
    location: *const c_char,
    location_len: usize,
    message: *const c_char,
    message_len: usize,
) -> ! {
    // SAFETY: both arguments follow the header contract (NUL terminated or sized).
    let loc = String::from_utf8_lossy(unsafe { env::cstr(location, location_len) }).into_owned();
    let msg = String::from_utf8_lossy(unsafe { env::cstr(message, message_len) }).into_owned();
    eprintln!("FATAL ERROR: {loc} {msg}");
    std::process::abort()
}

/// `napi_fatal_exception`
///
/// # Safety
/// `err` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_fatal_exception(env: napi_env, err: napi_value) -> napi_status {
    guard(env, |e| {
        let Some(v) = (unsafe { env::value(err) }) else { return sys::napi_invalid_arg };
        let v = v.clone();
        let mut ctx = e.ctx.clone();
        let text = v.to_string(&mut ctx).map(|s| s.to_std_string_lossy()).unwrap_or_default();
        eprintln!("Uncaught (in native addon): {text}");
        let _ = e;
        // The addon cannot recover from an uncaught exception; end the process like Node does.
        std::process::abort()
    })
}

/// `napi_adjust_external_memory`
///
/// # Safety
/// `adjusted_value` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_adjust_external_memory(env: napi_env, change_in_bytes: i64, adjusted_value: *mut i64) -> napi_status {
    guard(env, |e| {
        if adjusted_value.is_null() {
            return sys::napi_invalid_arg;
        }
        let _ = e;
        unsafe { *adjusted_value = change_in_bytes };
        sys::napi_ok
    })
}

/// `napi_get_uv_event_loop`: pi-rs has no libuv, so there is no loop to hand out.
///
/// # Safety
/// `loop_` must be writable when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_uv_event_loop(env: napi_env, loop_: *mut *mut sys::uv_loop_s) -> napi_status {
    guard(env, |e| {
        let _ = e;
        if loop_.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *loop_ = std::ptr::null_mut() };
        sys::napi_ok
    })
}

/// `napi_async_init`
///
/// # Safety
/// `resource_name` must be a live handle; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_async_init(
    env: napi_env,
    _async_resource: napi_value,
    _async_resource_name: napi_value,
    result: *mut sys::napi_async_context,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        // An async context carries no state for us; hand out a unique token.
        unsafe { *result = e.next_id() as usize as *mut c_void };
        sys::napi_ok
    })
}

/// `napi_async_destroy`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_async_destroy(_env: napi_env, _ctx: sys::napi_async_context) -> napi_status {
    sys::napi_ok
}

/// `napi_make_callback`
///
/// # Safety
/// `argv` must hold `argc` handles; `result` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_make_callback(
    env: napi_env,
    _ctx: sys::napi_async_context,
    recv: napi_value,
    func: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    unsafe { crate::function::napi_call_function(env, recv, func, argc, argv, result) }
}

/// `napi_open_callback_scope`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_open_callback_scope(
    env: napi_env,
    _resource_object: napi_value,
    _ctx: sys::napi_async_context,
    result: *mut napi_callback_scope,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.next_id() as usize as *mut c_void };
        sys::napi_ok
    })
}

/// `napi_close_callback_scope`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_close_callback_scope(_env: napi_env, _scope: napi_callback_scope) -> napi_status {
    sys::napi_ok
}

/// `napi_create_async_work`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_async_work(
    env: napi_env,
    resource: napi_value,
    resource_name: napi_value,
    execute: sys::napi_async_execute_callback,
    complete: sys::napi_async_complete_callback,
    data: *mut c_void,
    result: *mut sys::napi_async_work,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || execute.is_none() {
            return sys::napi_invalid_arg;
        }
        let work = Box::new(sys::napi_async_work_data { resource, resource_name, execute, complete, data });
        unsafe { *result = Box::into_raw(work).cast() };
        let _ = e;
        sys::napi_ok
    })
}

/// `napi_delete_async_work`
///
/// # Safety
/// `work` must come from `napi_create_async_work`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_delete_async_work(env: napi_env, work: sys::napi_async_work) -> napi_status {
    guard(env, |e| {
        if work.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the work item was created by `napi_create_async_work`.
        unsafe { drop(Box::from_raw(work.cast::<sys::napi_async_work_data>())) };
        let _ = e;
        sys::napi_ok
    })
}

/// `napi_queue_async_work`
///
/// shortcut: pi-rs has no worker pool, so `execute` runs inline on the JS thread and `complete`
/// follows immediately; upgrade when addons need real async work (PKGP follow-up).
///
/// # Safety
/// `work` must come from `napi_create_async_work`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_queue_async_work(env: napi_env, work: sys::napi_async_work) -> napi_status {
    guard(env, |e| {
        if work.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the work item was created by `napi_create_async_work` and is still alive.
        let w = unsafe { &*work.cast::<sys::napi_async_work_data>() };
        let (execute, complete, data) = (w.execute, w.complete, w.data);
        if let Some(execute) = execute {
            // SAFETY: the addon's own execute callback over its own data.
            unsafe { execute(env, data) };
        }
        if let Some(complete) = complete {
            // SAFETY: the addon's own complete callback; it runs on the JS thread by contract.
            unsafe { complete(env, sys::napi_ok, data) };
        }
        let _ = e;
        sys::napi_ok
    })
}

/// `napi_cancel_async_work`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_cancel_async_work(env: napi_env, work: sys::napi_async_work) -> napi_status {
    guard(env, |e| {
        if work.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the work item was created by `napi_create_async_work`.
        let w = unsafe { &*work.cast::<sys::napi_async_work_data>() };
        if let Some(complete) = w.complete {
            // SAFETY: the addon's own complete callback.
            unsafe { complete(env, sys::napi_cancelled, w.data) };
        }
        let _ = e;
        sys::napi_ok
    })
}

/// `node_api_create_async_cleanup_hook`: runs the hook immediately (nothing is pending here).
///
/// # Safety
/// `fun` must be callable with `arg`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_async_cleanup_hook_begin(
    env: napi_env,
    fun: sys::napi_async_cleanup_hook,
    _arg: *mut c_void,
    result: *mut sys::napi_async_cleanup_hook_handle,
) -> napi_status {
    guard(env, |e| {
        if result.is_null() || fun.is_none() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.next_id() as usize as *mut c_void };
        sys::napi_ok
    })
}

/// `node_api_post_finalizer` is not needed without a GC-thread split, but keep the symbol.
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_post_finalizer(
    env: napi_env,
    finalize_cb: sys::napi_finalize,
    data: *mut c_void,
    hint: *mut c_void,
) -> napi_status {
    guard(env, |e| {
        if let Some(cb) = finalize_cb {
            // SAFETY: the addon's own finalizer.
            unsafe { cb(env.cast(), data, hint) };
        }
        let _ = e;
        sys::napi_ok
    })
}
