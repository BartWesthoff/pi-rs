//! Errors, exceptions and error factories.

use std::ffi::{c_char, c_void};

use pi_quickjs::{JsNativeError, JsString, JsValue};

use crate::env::{self, guard, guard_js, value};
use crate::sys::{self, napi_env, napi_status, napi_value};

/// The error constructor QuickJS uses for `kind`.
fn constructor(env: &mut env::Env, kind: &str) -> Option<JsValue> {
    let mut ctx = env.ctx.clone();
    ctx.eval(pi_quickjs::Source::from_bytes(kind.as_bytes())).ok()
}

/// Build an `Error` object of `kind` with `code`, `msg` and, when given, `cause`.
fn make_error(env: &mut env::Env, kind: &str, code: Option<&str>, msg: &str) -> Option<JsValue> {
    let ctor = constructor(env, kind)?;
    let ctor = ctor.as_function()?;
    let mut ctx = env.ctx.clone();
    let obj = ctor.construct(&[JsString::from(msg).into()], None, &mut ctx).ok()?;
    if let Some(code) = code {
        let _ = obj.set("code", JsString::from(code), true, &mut ctx);
    }
    Some(obj.into())
}

/// `napi_throw_error`
///
/// # Safety
/// `code` and `msg` must be valid C strings (either may be null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_throw_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status {
    guard(env, |e| {
        if msg.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the caller passes NUL terminated strings.
        let code = (!code.is_null()).then(|| String::from_utf8_lossy(unsafe { env::cstr(code, usize::MAX) }).into_owned());
        let msg = String::from_utf8_lossy(unsafe { env::cstr(msg, usize::MAX) });
        match make_error(e, "Error", code.as_deref(), &msg) {
            Some(v) => {
                e.throw_value(v);
                sys::napi_ok
            }
            None => sys::napi_generic_failure,
        }
    })
}

/// `napi_throw_type_error`
///
/// # Safety
/// `code` and `msg` must be valid C strings (either may be null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_throw_type_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status {
    guard(env, |e| {
        if msg.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the caller passes NUL terminated strings.
        let code = (!code.is_null()).then(|| String::from_utf8_lossy(unsafe { env::cstr(code, usize::MAX) }).into_owned());
        let msg = String::from_utf8_lossy(unsafe { env::cstr(msg, usize::MAX) });
        match make_error(e, "TypeError", code.as_deref(), &msg) {
            Some(v) => {
                e.throw_value(v);
                sys::napi_ok
            }
            None => sys::napi_generic_failure,
        }
    })
}

/// `napi_throw_range_error`
///
/// # Safety
/// `code` and `msg` must be valid C strings (either may be null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_throw_range_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status {
    guard(env, |e| {
        if msg.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the caller passes NUL terminated strings.
        let code = (!code.is_null()).then(|| String::from_utf8_lossy(unsafe { env::cstr(code, usize::MAX) }).into_owned());
        let msg = String::from_utf8_lossy(unsafe { env::cstr(msg, usize::MAX) });
        match make_error(e, "RangeError", code.as_deref(), &msg) {
            Some(v) => {
                e.throw_value(v);
                sys::napi_ok
            }
            None => sys::napi_generic_failure,
        }
    })
}

/// `napi_create_error`
///
/// # Safety
/// `code` and `msg` follow the N-API contract; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status {
    create_error_kind(env, "Error", code, msg, result)
}

/// `napi_create_type_error`
///
/// # Safety
/// `code` and `msg` follow the N-API contract; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_type_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status {
    create_error_kind(env, "TypeError", code, msg, result)
}

/// `napi_create_range_error`
///
/// # Safety
/// `code` and `msg` follow the N-API contract; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_range_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status {
    create_error_kind(env, "RangeError", code, msg, result)
}

fn create_error_kind(env: napi_env, kind: &str, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(msg_v) = (unsafe { value(msg) }) else { return sys::napi_invalid_arg };
        let Some(text) = (if msg_v.is_string() { msg_v.as_string() } else { None }) else {
            return sys::napi_string_expected;
        };
        let code_text = if code.is_null() {
            None
        } else {
            match unsafe { value(code) } {
                Some(v) if v.is_string() => v.as_string(),
                Some(v) if v.is_undefined() => None,
                _ => return sys::napi_string_expected,
            }
        };
        let Some(v) = make_error(e, kind, code_text.as_ref().map(|c| c.to_std_string_lossy()).as_deref(), &text.to_std_string_lossy()) else {
            return sys::napi_generic_failure;
        };
        let h = e.handle(v);
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_create_promise`
///
/// # Safety
/// `deferred` and `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_promise(env: napi_env, deferred: *mut sys::napi_deferred, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if deferred.is_null() || result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        let (promise, resolving) = pi_quickjs::JsPromise::new_pending(&mut ctx);
        let id = e.next_id();
        e.deferreds.borrow_mut().insert(id, (resolving.resolve.into(), resolving.reject.into()));
        unsafe {
            *deferred = id as usize as *mut c_void;
            *result = e.handle(promise.into());
        }
        sys::napi_ok
    })
}

/// Settle the promise behind `deferred`.
fn settle(env: &mut env::Env, deferred: sys::napi_deferred, value_v: napi_value, reject: bool) -> napi_status {
    if deferred.is_null() {
        return sys::napi_invalid_arg;
    }
    let Some(v) = (unsafe { value(value_v) }) else { return sys::napi_invalid_arg };
    let v = v.clone();
    let id = deferred as usize as u32;
    let Some((resolve, reject_fn)) = env.deferreds.borrow_mut().remove(&id) else {
        return sys::napi_invalid_arg;
    };
    let f = if reject { reject_fn } else { resolve };
    let mut ctx = env.ctx.clone();
    match f.as_function() {
        Some(f) => match f.call(&JsValue::undefined(), &[v], &mut ctx) {
            Ok(_) => sys::napi_ok,
            Err(err) => env::park(env, err),
        },
        None => sys::napi_generic_failure,
    }
}

/// `napi_resolve_deferred`
///
/// # Safety
/// `deferred` must come from `napi_create_promise`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_resolve_deferred(env: napi_env, deferred: sys::napi_deferred, resolution: napi_value) -> napi_status {
    guard(env, |e| settle(e, deferred, resolution, false))
}

/// `napi_reject_deferred`
///
/// # Safety
/// `deferred` must come from `napi_create_promise`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_reject_deferred(env: napi_env, deferred: sys::napi_deferred, rejection: napi_value) -> napi_status {
    guard(env, |e| settle(e, deferred, rejection, true))
}

/// `napi_is_promise`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_promise(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |_e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let is = v.is_promise();
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_create_date`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_date(env: napi_env, time: f64, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        let Some(ctor) = ctx.eval(pi_quickjs::Source::from_bytes(b"Date")).ok().and_then(|v| v.as_function()) else {
            return sys::napi_generic_failure;
        };
        match ctor.construct(&[JsValue::from(time)], None, &mut ctx) {
            Ok(d) => {
                let h = e.handle(d.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_is_date`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_date(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else {
            unsafe { *result = false };
            return sys::napi_ok;
        };
        let mut ctx = e.ctx.clone();
        let is = is_date(&mut ctx, &obj);
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_get_date_value`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_date_value(env: napi_env, value_: napi_value, result: *mut f64) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_date_expected };
        let mut ctx = e.ctx.clone();
        if !is_date(&mut ctx, &obj) {
            return sys::napi_date_expected;
        }
        let Some(get_time) = obj.get("getTime", &mut ctx).ok().and_then(|f| f.as_function()) else {
            return sys::napi_date_expected;
        };
        match get_time.call(&obj.into(), &[], &mut ctx).and_then(|n| n.to_number(&mut ctx)) {
            Ok(n) => {
                unsafe { *result = n };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `node_api_create_syntax_error`
///
/// # Safety
/// `code` and `msg` follow the N-API contract; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_syntax_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status {
    create_error_kind(env, "SyntaxError", code, msg, result)
}

/// `node_api_throw_syntax_error`
///
/// # Safety
/// `code` and `msg` must be valid C strings (either may be null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_throw_syntax_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status {
    guard(env, |e| {
        if msg.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the caller passes NUL terminated strings.
        let code = (!code.is_null()).then(|| String::from_utf8_lossy(unsafe { env::cstr(code, usize::MAX) }).into_owned());
        let msg = String::from_utf8_lossy(unsafe { env::cstr(msg, usize::MAX) });
        match make_error(e, "SyntaxError", code.as_deref(), &msg) {
            Some(v) => {
                e.throw_value(v);
                sys::napi_ok
            }
            None => sys::napi_generic_failure,
        }
    })
}

/// Is `obj` a `Date`?
fn is_date(ctx: &mut pi_quickjs::Context, obj: &pi_quickjs::JsObject) -> bool {
    // `Object.prototype.toString.call(obj) === "[object Date]"` also matches subclasses, like V8.
    ctx.eval(pi_quickjs::Source::from_bytes(b"Object.prototype.toString"))
        .ok()
        .and_then(|f| f.as_function())
        .and_then(|f| f.call(&JsValue::undefined(), &[JsValue::from(obj.clone())], ctx).ok())
        .and_then(|s| s.as_string())
        .is_some_and(|s| s.to_std_string_lossy() == "[object Date]")
}

/// Unused: keeps `JsNativeError` in scope for the error helpers.
#[allow(dead_code)]
fn _unused(_: JsNativeError) {}
