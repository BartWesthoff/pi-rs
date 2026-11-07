//! `napi_value` creation and inspection: primitives, strings, coercion and `typeof`.

use std::ffi::{c_char, c_void};

use pi_quickjs::{JsString, JsValue};

use crate::env::{self, guard, guard_js, value};
use crate::sys::{self, napi_env, napi_status, napi_value, napi_valuetype};

/// `napi_get_undefined`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_undefined(env: napi_env, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::undefined()) };
        sys::napi_ok
    })
}

/// `napi_get_null`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_null(env: napi_env, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::null()) };
        sys::napi_ok
    })
}

/// `napi_get_boolean`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_boolean(env: napi_env, val: bool, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::from(val)) };
        sys::napi_ok
    })
}

/// `napi_create_double`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_double(env: napi_env, val: f64, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::from(val)) };
        sys::napi_ok
    })
}

/// `napi_create_int32`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_int32(env: napi_env, val: i32, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::from(val)) };
        sys::napi_ok
    })
}

/// `napi_create_uint32`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_uint32(env: napi_env, val: u32, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::from(val)) };
        sys::napi_ok
    })
}

/// `napi_create_int64`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_int64(env: napi_env, val: i64, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.handle(JsValue::from(val as f64)) };
        sys::napi_ok
    })
}

/// `napi_create_string_utf8`
///
/// # Safety
/// `str` must point to `length` UTF-8 bytes (or be NUL terminated with `NAPI_AUTO_LENGTH`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_string_utf8(
    env: napi_env,
    str_: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        let status = check_new_string_args(str_.is_null(), length, result);
        if status != sys::napi_ok {
            return status;
        }
        let s = String::from_utf8_lossy(unsafe { env::cstr(str_, length) }).into_owned();
        unsafe { *result = e.handle(JsString::from(s).into()) };
        sys::napi_ok
    })
}

/// `napi_create_string_latin1`
///
/// # Safety
/// `str` must point to `length` Latin-1 bytes (or be NUL terminated with `NAPI_AUTO_LENGTH`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_string_latin1(
    env: napi_env,
    str_: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        let status = check_new_string_args(str_.is_null(), length, result);
        if status != sys::napi_ok {
            return status;
        }
        let bytes = unsafe { env::cstr(str_, length) };
        let units: Vec<u16> = bytes.iter().map(|b| u16::from(*b)).collect();
        unsafe { *result = e.handle(JsString::from_utf16(&units).into()) };
        sys::napi_ok
    })
}

/// `napi_create_string_utf16`
///
/// # Safety
/// `str` must point to `length` UTF-16 code units (or be NUL terminated with
/// `NAPI_AUTO_LENGTH`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_string_utf16(
    env: napi_env,
    str_: *const u16,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        let status = check_new_string_args(str_.is_null(), length, result);
        if status != sys::napi_ok {
            return status;
        }
        let mut units = Vec::new();
        if length == usize::MAX {
            let mut i = 0;
            // SAFETY: NUL terminated per the header contract.
            while unsafe { *str_.add(i) } != 0 {
                units.push(unsafe { *str_.add(i) });
                i += 1;
            }
        } else {
            // SAFETY: the caller promises `length` readable code units.
            units.extend_from_slice(unsafe { std::slice::from_raw_parts(str_, length) });
        }
        unsafe { *result = e.handle(JsString::from_utf16(&units).into()) };
        sys::napi_ok
    })
}

/// `napi_typeof`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_typeof(env: napi_env, val: napi_value, result: *mut napi_valuetype) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let t = if v.is_undefined() {
            sys::napi_undefined
        } else if v.is_null() {
            sys::napi_null
        } else if v.is_boolean() {
            sys::napi_boolean
        } else if v.is_number() {
            sys::napi_number
        } else if v.is_string() {
            sys::napi_string
        } else if v.is_symbol() {
            sys::napi_symbol
        } else if v.is_bigint() {
            sys::napi_bigint
        } else if v.is_callable() {
            sys::napi_function
        } else if crate::wrap::is_external(e, v) {
            sys::napi_external
        } else {
            sys::napi_object
        };
        unsafe { *result = t };
        sys::napi_ok
    })
}

/// `napi_get_value_double`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_double(env: napi_env, val: napi_value, result: *mut f64) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let Some(n) = v.as_number() else { return sys::napi_number_expected };
        let _ = e;
        unsafe { *result = n };
        sys::napi_ok
    })
}

/// `napi_get_value_int32`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_int32(env: napi_env, val: napi_value, result: *mut i32) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        if !v.is_number() {
            return sys::napi_number_expected;
        }
        let mut ctx = e.ctx.clone();
        let Some(n) = v.as_number() else { return sys::napi_number_expected };
        let _ = &mut ctx;
        // Node truncates like a C cast (ToInt32): modular, NaN/Infinity → 0.
        let n = if n.is_finite() { n.trunc() } else { 0.0 };
        let n = n.rem_euclid(4_294_967_296.0).rem_euclid(4_294_967_296.0);
        let n = if n >= 2_147_483_648.0 { n - 4_294_967_296.0 } else { n };
        unsafe { *result = n as i32 };
        sys::napi_ok
    })
}

/// `napi_get_value_uint32`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_uint32(env: napi_env, val: napi_value, result: *mut u32) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        if !v.is_number() {
            return sys::napi_number_expected;
        }
        let Some(n) = v.as_number() else { return sys::napi_number_expected };
        let _ = e;
        let n = if n.is_finite() { n.trunc() } else { 0.0 };
        unsafe { *result = n.rem_euclid(4_294_967_296.0) as u32 };
        sys::napi_ok
    })
}

/// `napi_get_value_int64`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_int64(env: napi_env, val: napi_value, result: *mut i64) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        if !v.is_number() {
            return sys::napi_number_expected;
        }
        let Some(n) = v.as_number() else { return sys::napi_number_expected };
        let _ = e;
        unsafe { *result = if n.is_finite() { n.trunc() as i64 } else { 0 } };
        sys::napi_ok
    })
}

/// `napi_get_value_bool`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_bool(env: napi_env, val: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let _ = e;
        if !v.is_boolean() {
            return sys::napi_boolean_expected;
        }
        unsafe { *result = v.to_boolean() };
        sys::napi_ok
    })
}

/// The UTF-8 bytes of `v`, or `None` when it is not a string.
/// Node's `CHECK_NEW_STRING_ARGS`: a null `str` is only an error when `length > 0`, and a
/// length past `INT_MAX` fails with `napi_invalid_arg`.
fn check_new_string_args(str_is_null: bool, length: usize, result: *mut napi_value) -> napi_status {
    if (length > 0 && str_is_null) || result.is_null() {
        return sys::napi_invalid_arg;
    }
    if length != usize::MAX && length > i32::MAX as usize {
        return sys::napi_invalid_arg;
    }
    sys::napi_ok
}

fn utf8_of(v: &JsValue) -> Option<Vec<u8>> {
    let s = v.as_string()?;
    Some(s.to_std_string_lossy().into_bytes())
}

/// `napi_get_value_string_utf8`
///
/// # Safety
/// `buf` must be writable for `bufsize` bytes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_string_utf8(
    env: napi_env,
    val: napi_value,
    buf: *mut c_char,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let Some(v) = v else { return sys::napi_invalid_arg };
        let _ = e;
        let Some(bytes) = utf8_of(v) else { return sys::napi_string_expected };
        if buf.is_null() && result.is_null() {
            return sys::napi_invalid_arg;
        }
        let copied = copy_utf8(&bytes, buf.cast::<u8>(), bufsize);
        if !result.is_null() {
            unsafe { *result = copied };
        }
        sys::napi_ok
    })
}

/// Like [`copy_bytes`], but never splits a UTF-8 sequence (what V8's `WriteUtf8` does).
fn copy_utf8(bytes: &[u8], buf: *mut u8, bufsize: usize) -> usize {
    if buf.is_null() || bufsize == 0 {
        return bytes.len();
    }
    let mut n = bytes.len().min(bufsize - 1);
    while n > 0 && n < bytes.len() && (bytes[n] & 0xc0) == 0x80 {
        n -= 1;
    }
    // SAFETY: the caller promises `bufsize` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    n
}

/// Copy `bytes` into `buf` (NUL terminated, at most `bufsize` bytes) and report the number of
/// bytes written, or the byte length when `buf` is null / `bufsize` is 0.
fn copy_bytes(bytes: &[u8], buf: *mut u8, bufsize: usize) -> usize {
    if buf.is_null() || bufsize == 0 {
        return bytes.len();
    }
    let n = bytes.len().min(bufsize - 1);
    // SAFETY: the caller promises `bufsize` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    n
}

/// `napi_get_value_string_latin1`
///
/// # Safety
/// `buf` must be writable for `bufsize` bytes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_string_latin1(
    env: napi_env,
    val: napi_value,
    buf: *mut c_char,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let Some(v) = v else { return sys::napi_invalid_arg };
        let _ = e;
        let Some(s) = v.as_string() else { return sys::napi_string_expected };
        if buf.is_null() && result.is_null() {
            return sys::napi_invalid_arg;
        }
        let units = s.to_utf16();
        let bytes: Vec<u8> = units.iter().map(|u| (*u & 0xff) as u8).collect();
        let copied = copy_bytes(&bytes, buf.cast::<u8>(), bufsize);
        if !result.is_null() {
            unsafe { *result = copied };
        }
        sys::napi_ok
    })
}

/// `napi_get_value_string_utf16`
///
/// # Safety
/// `buf` must be writable for `bufsize` code units when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_string_utf16(
    env: napi_env,
    val: napi_value,
    buf: *mut u16,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let Some(v) = v else { return sys::napi_invalid_arg };
        let _ = e;
        let Some(s) = v.as_string() else { return sys::napi_string_expected };
        if buf.is_null() && result.is_null() {
            return sys::napi_invalid_arg;
        }
        let units = s.to_utf16();
        let copied = if buf.is_null() || bufsize == 0 {
            units.len()
        } else {
            let n = units.len().min(bufsize - 1);
            // SAFETY: the caller promises `bufsize` writable units.
            unsafe {
                std::ptr::copy_nonoverlapping(units.as_ptr(), buf, n);
                *buf.add(n) = 0;
            }
            n
        };
        if !result.is_null() {
            unsafe { *result = copied };
        }
        sys::napi_ok
    })
}

/// `napi_coerce_to_bool`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_coerce_to_bool(env: napi_env, val: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let b = v.to_boolean();
        unsafe { *result = e.handle(JsValue::from(b)) };
        sys::napi_ok
    })
}

/// `napi_coerce_to_number`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_coerce_to_number(env: napi_env, val: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        match v.to_number(&mut ctx) {
            Ok(n) => {
                let h = e.handle(JsValue::from(n));
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_coerce_to_object`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_coerce_to_object(env: napi_env, val: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        match v.to_object(&mut ctx) {
            Ok(o) => {
                let h = e.handle(o.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_coerce_to_string`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_coerce_to_string(env: napi_env, val: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        match v.to_string(&mut ctx) {
            Ok(s) => {
                let h = e.handle(s.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_strict_equals`
///
/// # Safety
/// Both values must be live handles and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_strict_equals(
    env: napi_env,
    lhs: napi_value,
    rhs: napi_value,
    result: *mut bool,
) -> napi_status {
    guard_js(env, |e| {
        let (a, b) = unsafe { (value(lhs), value(rhs)) };
        let (Some(a), Some(b), false) = (a, b, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let _ = e;
        unsafe { *result = a.strict_equals(b) };
        sys::napi_ok
    })
}

/// `napi_create_bigint_int64`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_bigint_int64(env: napi_env, val: i64, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let ctx = e.ctx.as_raw();
        let v = unsafe { JsValue::from_raw(pi_quickjs::rquickjs::qjs::JS_NewBigInt64(ctx, val)) };
        unsafe { *result = e.handle(v) };
        sys::napi_ok
    })
}

/// `napi_create_bigint_uint64`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_bigint_uint64(env: napi_env, val: u64, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let ctx = e.ctx.as_raw();
        let v = unsafe { JsValue::from_raw(pi_quickjs::rquickjs::qjs::JS_NewBigUint64(ctx, val)) };
        unsafe { *result = e.handle(v) };
        sys::napi_ok
    })
}

/// `napi_get_value_bigint_int64`
///
/// # Safety
/// `val` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_bigint_int64(
    env: napi_env,
    val: napi_value,
    result: *mut i64,
    lossless: *mut bool,
) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(val) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let _ = e;
        let Some(b) = v.as_bigint() else { return sys::napi_bigint_expected };
        let n = b.to_i64();
        let exact = true;
        unsafe {
            *result = n;
            if !lossless.is_null() {
                *lossless = exact;
            }
        }
        sys::napi_ok
    })
}

/// `napi_create_array`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_array(env: napi_env, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        let a = pi_quickjs::JsArray::new(&mut ctx);
        unsafe { *result = e.handle(a.into()) };
        sys::napi_ok
    })
}

/// `napi_create_array_with_length`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_array_with_length(env: napi_env, length: usize, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        let a = pi_quickjs::JsArray::new(&mut ctx);
        let raw = a.as_raw();
        // SAFETY: `raw` is an array of this context.
        unsafe { pi_quickjs::rquickjs::qjs::JS_SetLength(ctx.as_raw(), raw, length as i64) };
        unsafe { *result = e.handle(a.into()) };
        sys::napi_ok
    })
}

/// `napi_create_object`
///
/// # Safety
/// `result` must be a writable `napi_value*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_object(env: napi_env, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let ctx = e.ctx.clone();
        let obj = pi_quickjs::JsObject::with_object_proto(ctx.intrinsics());
        unsafe { *result = e.handle(obj.into()) };
        sys::napi_ok
    })
}

/// `napi_create_symbol`
///
/// # Safety
/// `description` may be `NULL`; `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_symbol(env: napi_env, description: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let desc = unsafe { value(description) }.and_then(|v| v.as_string());
        let Some(sym) = pi_quickjs::JsSymbol::new(desc) else { return sys::napi_generic_failure };
        unsafe { *result = e.handle(sym.into()) };
        sys::napi_ok
    })
}

/// `node_api_symbol_for`
///
/// # Safety
/// `utf8description` must be NUL terminated (or `length` bytes long); `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_symbol_for(
    env: napi_env,
    utf8description: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || utf8description.is_null() {
            return sys::napi_invalid_arg;
        }
        let desc = String::from_utf8_lossy(unsafe { env::cstr(utf8description, length) }).into_owned();
        let mut ctx = e.ctx.clone();
        let sym_for = ctx.eval(pi_quickjs::Source::from_bytes(b"Symbol.for")).ok().and_then(|v| v.as_function());
        let Some(sym_for) = sym_for else { return sys::napi_generic_failure };
        match sym_for.call(&JsValue::undefined(), &[JsString::from(desc).into()], &mut ctx) {
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_get_version`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_version(_env: napi_env, result: *mut u32) -> napi_status {
    if result.is_null() {
        return sys::napi_invalid_arg;
    }
    // SAFETY: checked above.
    unsafe { *result = 8 };
    sys::napi_ok
}

/// `napi_get_node_version`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_node_version(_env: napi_env, result: *mut *const sys::napi_node_version) -> napi_status {
    // SAFETY: the contents are immutable and the pointer is to a `'static` C string.
    struct Version(sys::napi_node_version);
    unsafe impl Sync for Version {}
    static VERSION: Version = Version(sys::napi_node_version {
        major: 22,
        minor: 19,
        patch: 0,
        release: c"node".as_ptr(),
    });
    if result.is_null() {
        return sys::napi_invalid_arg;
    }
    // SAFETY: checked above; the value is static.
    unsafe { *result = &VERSION.0 };
    sys::napi_ok
}

/// `napi_throw`
///
/// # Safety
/// `error` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_throw(env: napi_env, error: napi_value) -> napi_status {
    guard(env, |e| {
        let Some(v) = (unsafe { value(error) }) else { return sys::napi_invalid_arg };
        let v = v.clone();
        e.throw_value(v);
        sys::napi_ok
    })
}

/// `napi_is_error`
///
/// # Safety
/// `value` must be a live handle and `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_error(env: napi_env, value: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { crate::env::value(value) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let _ = e;
        let is = v.as_object().is_some_and(|o| o.is_error());
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_is_exception_pending`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_exception_pending(env: napi_env, result: *mut bool) -> napi_status {
    guard(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *result = e.has_exception() };
        sys::napi_ok
    })
}

/// `napi_get_and_clear_last_exception`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_and_clear_last_exception(env: napi_env, result: *mut napi_value) -> napi_status {
    guard(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let v = e.take_exception();
        let h = e.handle(v);
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_get_last_error_info`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_last_error_info(
    env: napi_env,
    result: *mut *const sys::napi_extended_error_info,
) -> napi_status {
    if env.is_null() || result.is_null() {
        return sys::napi_invalid_arg;
    }
    // SAFETY: the environment is live (owned by the loader).
    let e = unsafe { &*env.cast::<crate::env::Env>() };
    e.set_last(e.last_status());
    unsafe { *result = e.info() };
    sys::napi_ok
}

/// `napi_get_global`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_global(env: napi_env, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let g = e.ctx.global_object();
        unsafe { *result = e.handle(g.into()) };
        sys::napi_ok
    })
}

/// Unused placeholder so `c_void` stays in scope for the module's signatures.
#[allow(dead_code)]
fn _unused(_: *mut c_void) {}

/// `node_api_create_property_key_latin1`: like `napi_create_string_latin1` (pi-rs always
/// copies, so the key is an ordinary string).
///
/// # Safety
/// `str` must point to `length` Latin-1 bytes (or be NUL terminated).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_property_key_latin1(
    env: napi_env,
    str_: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    // SAFETY: forwarded with the caller's contract.
    unsafe { napi_create_string_latin1(env, str_, length, result) }
}

/// `node_api_create_property_key_utf8`: like `napi_create_string_utf8`.
///
/// # Safety
/// `str` must point to `length` UTF-8 bytes (or be NUL terminated).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_property_key_utf8(
    env: napi_env,
    str_: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    // SAFETY: forwarded with the caller's contract.
    unsafe { napi_create_string_utf8(env, str_, length, result) }
}

/// `node_api_create_property_key_utf16`: like `napi_create_string_utf16`.
///
/// # Safety
/// `str` must point to `length` UTF-16 code units (or be NUL terminated).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_property_key_utf16(
    env: napi_env,
    str_: *const u16,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    // SAFETY: forwarded with the caller's contract.
    unsafe { napi_create_string_utf16(env, str_, length, result) }
}

/// `node_api_create_external_string_latin1`
///
/// shortcut: QuickJS strings always own their bytes, so the text is copied and the addon's
/// finalizer runs immediately with `*copied = false`; upgrade when a zero-copy string exists.
///
/// # Safety
/// `str` must point to `length` readable bytes; `copied` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_external_string_latin1(
    env: napi_env,
    str_: *mut c_char,
    length: usize,
    finalize_callback: sys::napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    // SAFETY: forwarded with the caller's contract.
    let status = unsafe { napi_create_string_latin1(env, str_, length, result) };
    if status == sys::napi_ok {
        unsafe {
            if !copied.is_null() {
                *copied = false;
            }
            if let Some(cb) = finalize_callback {
                cb(env, str_.cast(), finalize_hint);
            }
        }
    }
    status
}

/// `node_api_create_external_string_utf16`
///
/// shortcut: QuickJS strings always own their bytes, so the text is copied and the addon's
/// finalizer runs immediately with `*copied = false`; upgrade when a zero-copy string exists.
///
/// # Safety
/// `str` must point to `length` readable code units; `copied` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_external_string_utf16(
    env: napi_env,
    str_: *mut u16,
    length: usize,
    finalize_callback: sys::napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    // SAFETY: forwarded with the caller's contract.
    let status = unsafe { napi_create_string_utf16(env, str_, length, result) };
    if status == sys::napi_ok {
        unsafe {
            if !copied.is_null() {
                *copied = false;
            }
            if let Some(cb) = finalize_callback {
                cb(env, str_.cast(), finalize_hint);
            }
        }
    }
    status
}

/// `node_api_create_object_with_properties`
///
/// # Safety
/// The name/value arrays must hold `property_count` live handles; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_create_object_with_properties(
    env: napi_env,
    prototype_or_null: napi_value,
    property_names: *const napi_value,
    property_values: *const napi_value,
    property_count: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || (property_count > 0 && (property_names.is_null() || property_values.is_null())) {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        // Node maps a NULL pointer and a JS null alike to a null prototype.
        let proto = if prototype_or_null.is_null() {
            None
        } else {
            match unsafe { value(prototype_or_null) } {
                Some(v) if v.is_null() => None,
                Some(v) => match v.as_object() {
                    Some(p) => Some(p),
                    None => return sys::napi_object_expected,
                },
                None => return sys::napi_invalid_arg,
            }
        };
        let obj = match proto {
            Some(p) => pi_quickjs::JsObject::with_proto(Some(&p)),
            None => pi_quickjs::JsObject::with_null_proto(),
        };
        for i in 0..property_count {
            // SAFETY: the caller promises two arrays of `property_count` handles.
            let (Some(k), Some(v)) = (unsafe { value(*property_names.add(i)) }, unsafe { value(*property_values.add(i)) }) else {
                return sys::napi_invalid_arg;
            };
            let Some(key) = crate::object::key_of(e, k) else { return sys::napi_name_expected };
            if obj.set(key, v.clone(), true, &mut ctx).is_err() {
                return sys::napi_generic_failure;
            }
        }
        let h = e.handle(obj.into());
        unsafe { *result = h };
        sys::napi_ok
    })
}
