//! ArrayBuffer, TypedArray and DataView support.

use std::ffi::c_void;

use pi_quickjs::rquickjs::qjs;
use pi_quickjs::{JsObject, JsValue};

use crate::env::{self, guard_js, value};
use crate::sys::{self, napi_env, napi_status, napi_value, napi_typedarray_type};

/// The `ArrayBuffer` class id (for `JS_GetOpaque` of external buffers).
fn typed_array_enum(t: napi_typedarray_type) -> Option<qjs::JSTypedArrayEnum> {
    Some(match t {
        sys::napi_int8_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT8,
        sys::napi_uint8_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT8,
        sys::napi_uint8_clamped_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT8C,
        sys::napi_int16_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT16,
        sys::napi_uint16_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT16,
        sys::napi_int32_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT32,
        sys::napi_uint32_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT32,
        sys::napi_float16_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT16,
        sys::napi_float32_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT32,
        sys::napi_float64_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT64,
        sys::napi_bigint64_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_BIG_INT64,
        sys::napi_biguint64_array => qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_BIG_UINT64,
        _ => return None,
    })
}

/// The N-API type of a QuickJS typed array.
fn napi_type(t: i32) -> napi_typedarray_type {
    match t as u32 {
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT8 => sys::napi_int8_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT8 => sys::napi_uint8_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT8C => sys::napi_uint8_clamped_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT16 => sys::napi_int16_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT16 => sys::napi_uint16_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_INT32 => sys::napi_int32_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT32 => sys::napi_uint32_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT16 => sys::napi_float16_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT32 => sys::napi_float32_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_FLOAT64 => sys::napi_float64_array,
        x if x == qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_BIG_INT64 => sys::napi_bigint64_array,
        _ => sys::napi_biguint64_array,
    }
}

/// `napi_create_arraybuffer` (QuickJS copies the bytes and owns the memory).
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_arraybuffer(env: napi_env, byte_length: usize, data: *mut *mut c_void, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut bytes = vec![0u8; byte_length];
        let ctx = e.ctx.clone();
        // SAFETY: `JS_NewArrayBufferCopy` copies `byte_length` bytes from `bytes`.
        let v = unsafe { JsValue::from_raw(qjs::JS_NewArrayBufferCopy(ctx.as_raw(), bytes.as_mut_ptr(), byte_length as u64)) };
        if unsafe { qjs::JS_IsException(v.as_raw()) } {
            return env::park(e, pi_quickjs::take_exception(&ctx));
        }
        let Some(obj) = v.as_object() else { return sys::napi_generic_failure };
        if !data.is_null() {
            // SAFETY: `obj` is the ArrayBuffer we just created. `JS_GetArrayBuffer` always
            // writes its size out-parameter, so it must not be null.
            let mut size = 0u64;
            let base = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut size, obj.as_raw()) };
            unsafe { *data = base.cast() };
        }
        let h = e.handle(obj.into());
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_create_external_arraybuffer`: QuickJS cannot wrap foreign memory without a realloc
/// callback, so the bytes are copied and the finalizer still runs (the addon keeps its contract).
///
/// # Safety
/// `data` must point to `byte_length` readable bytes; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_external_arraybuffer(
    env: napi_env,
    data: *mut c_void,
    byte_length: usize,
    finalize_cb: sys::napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let status = unsafe { napi_create_arraybuffer(env, byte_length, std::ptr::null_mut(), result) };
    if status != sys::napi_ok {
        return status;
    }
    if data.is_null() {
        // Node reports an external buffer with no backing store as detached (and empty).
        return unsafe { napi_detach_arraybuffer(env, *result) };
    }
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(*result) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        let ctx = e.ctx.clone();
        // SAFETY: the caller promises `byte_length` readable bytes.
        let src = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), byte_length) };
        let mut bytes = src.to_vec();
        // Copy the caller's bytes into the (fresh) buffer.
        let mut dummy = 0u64;
        let buf = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut dummy, obj.as_raw()) };
        if !buf.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, byte_length) };
        }
        bytes.clear();
        if let Some(cb) = finalize_cb {
            let fin = crate::env::Finalizer { cb: Some(cb), data, hint: finalize_hint };
            add_finalizer(e, &obj, fin);
        }
        let _ = obj;
        sys::napi_ok
    })
}

/// Register a finalizer on `obj` and keep the token alive with the environment.
fn add_finalizer(env: &mut env::Env, obj: &JsObject, fin: crate::env::Finalizer) {
    let Some(registry) = env.registry() else { return };
    let mut ctx = env.ctx.clone();
    let id = env.next_id();
    env.finals.borrow_mut().insert(id, fin);
    let Ok(Some(register)) = registry.get("register", &mut ctx).map(|v| v.as_function()) else { return };
    let _ = register.call(&registry.clone().into(), &[obj.clone().into(), JsValue::from(id)], &mut ctx);
}

/// `napi_get_arraybuffer_info`
///
/// # Safety
/// `data` and `byte_length` may be null; otherwise writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_arraybuffer_info(env: napi_env, arraybuffer: napi_value, data: *mut *mut c_void, byte_length: *mut usize) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(arraybuffer) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            return sys::napi_invalid_arg;
        };
        let ctx = e.ctx.clone();
        let mut len: qjs::size_t = 0;
        // SAFETY: `obj` is an ArrayBuffer of this context.
        let base = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut len, obj.as_raw()) };
        if !data.is_null() {
            unsafe { *data = base.cast() };
        }
        if !byte_length.is_null() {
            unsafe { *byte_length = len as usize };
        }
        sys::napi_ok
    })
}

/// `napi_is_arraybuffer`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_arraybuffer(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let _ = e;
        let is = v.as_object().is_some_and(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) });
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_detach_arraybuffer`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_detach_arraybuffer(env: napi_env, arraybuffer: napi_value) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(arraybuffer) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            return sys::napi_invalid_arg;
        };
        let ctx = e.ctx.clone();
        // SAFETY: an ArrayBuffer of this context.
        unsafe { qjs::JS_DetachArrayBuffer(ctx.as_raw(), obj.as_raw()) };
        sys::napi_ok
    })
}

/// `napi_is_detached_arraybuffer`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_detached_arraybuffer(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            unsafe { *result = false };
            return sys::napi_ok;
        };
        let ctx = e.ctx.clone();
        let mut len: qjs::size_t = 0;
        // SAFETY: an ArrayBuffer of this context; a detached one reports a null base.
        let base = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut len, obj.as_raw()) };
        if base.is_null() {
            // QuickJS reports detachment as a TypeError; the probe must not leak it.
            let _ = pi_quickjs::take_exception(&ctx);
        }
        unsafe { *result = base.is_null() };
        let _ = e;
        sys::napi_ok
    })
}

/// `napi_create_typedarray`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_typedarray(
    env: napi_env,
    type_: napi_typedarray_type,
    length: usize,
    arraybuffer: napi_value,
    byte_offset: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(enum_) = typed_array_enum(type_) else { return sys::napi_invalid_arg };
        let Some(v) = (unsafe { value(arraybuffer) }) else { return sys::napi_invalid_arg };
        let Some(buf) = v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        // The constructor form `new <Kind>Array(buffer, byteOffset, length)`.
        let ctor_src = match type_ {
            sys::napi_int8_array => "Int8Array",
            sys::napi_uint8_array => "Uint8Array",
            sys::napi_uint8_clamped_array => "Uint8ClampedArray",
            sys::napi_int16_array => "Int16Array",
            sys::napi_uint16_array => "Uint16Array",
            sys::napi_int32_array => "Int32Array",
            sys::napi_uint32_array => "Uint32Array",
            sys::napi_float16_array => "Float16Array",
            sys::napi_float32_array => "Float32Array",
            sys::napi_bigint64_array => "BigInt64Array",
            sys::napi_biguint64_array => "BigUint64Array",
            _ => "Float64Array",
        };
        let Some(ctor) = ctx.eval(pi_quickjs::Source::from_bytes(ctor_src.as_bytes())).ok().and_then(|v| v.as_function()) else {
            return sys::napi_generic_failure;
        };
        let args = [JsValue::from(buf.clone()), JsValue::from(byte_offset), JsValue::from(length)];
        match ctor.construct(&args, None, &mut ctx) {
            Ok(a) => {
                let _ = enum_;
                let h = e.handle(a.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_create_dataview`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_dataview(
    env: napi_env,
    length: usize,
    arraybuffer: napi_value,
    byte_offset: usize,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(arraybuffer) }) else { return sys::napi_invalid_arg };
        let Some(buf) = v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        let Some(ctor) = ctx.eval(pi_quickjs::Source::from_bytes(b"DataView")).ok().and_then(|v| v.as_function()) else {
            return sys::napi_generic_failure;
        };
        let args = [JsValue::from(buf.clone()), JsValue::from(byte_offset), JsValue::from(length)];
        match ctor.construct(&args, None, &mut ctx) {
            Ok(a) => {
                let h = e.handle(a.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_get_typedarray_info`
///
/// # Safety
/// All out pointers may be null; otherwise writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_typedarray_info(
    env: napi_env,
    typedarray: napi_value,
    type_: *mut napi_typedarray_type,
    length: *mut usize,
    data: *mut *mut c_void,
    arraybuffer: *mut napi_value,
    byte_offset: *mut usize,
) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(typedarray) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object().filter(|o| unsafe { qjs::JS_GetTypedArrayType(o.as_raw()) >= 0 }) else {
            return sys::napi_invalid_arg;
        };
        let ctx = e.ctx.clone();
        let mut off: qjs::size_t = 0;
        let mut len: qjs::size_t = 0;
        let mut per: qjs::size_t = 0;
        // SAFETY: `obj` is a typed array of this context.
        let buf = unsafe { qjs::JS_GetTypedArrayBuffer(ctx.as_raw(), obj.as_raw(), &mut off, &mut len, &mut per) };
        let buf = unsafe { JsValue::from_raw(qjs::JS_DupValue(ctx.as_raw(), buf)) };
        let Some(buf_obj) = buf.as_object() else { return sys::napi_generic_failure };
        if !type_.is_null() {
            unsafe { *type_ = napi_type(qjs::JS_GetTypedArrayType(obj.as_raw())) };
        }
        if !length.is_null() {
            unsafe { *length = if per > 0 { len.checked_div(per).unwrap_or(0) as usize } else { 0 } };
        }
        if !byte_offset.is_null() {
            unsafe { *byte_offset = off as usize };
        }
        if !arraybuffer.is_null() {
            let h = e.handle(buf.clone());
            unsafe { *arraybuffer = h };
        }
        if !data.is_null() {
            let mut total: qjs::size_t = 0;
            // SAFETY: `buf_obj` is an ArrayBuffer.
            let base = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut total, buf_obj.as_raw()) };
            unsafe { *data = if base.is_null() { std::ptr::null_mut() } else { base.add(off as usize).cast() } };
        }
        sys::napi_ok
    })
}

/// `napi_is_typedarray`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_typedarray(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let _ = e;
        let is = v.as_object().is_some_and(|o| unsafe { qjs::JS_GetTypedArrayType(o.as_raw()) >= 0 });
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_is_dataview`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_dataview(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let _ = e;
        let is = v.as_object().is_some_and(|o| unsafe { qjs::JS_IsDataView(o.as_raw()) });
        unsafe { *result = is };
        sys::napi_ok
    })
}

/// `napi_get_dataview_info`
///
/// # Safety
/// All out pointers may be null; otherwise writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_dataview_info(
    env: napi_env,
    dataview: napi_value,
    byte_length: *mut usize,
    data: *mut *mut c_void,
    arraybuffer: *mut napi_value,
    byte_offset: *mut usize,
) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(dataview) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object().filter(|o| unsafe { qjs::JS_IsDataView(o.as_raw()) }) else {
            return sys::napi_invalid_arg;
        };
        let mut ctx = e.ctx.clone();
        let Some(buf_v) = obj.get("buffer", &mut ctx).ok() else { return sys::napi_generic_failure };
        let Some(buf_obj) = buf_v.as_object().filter(|o| unsafe { qjs::JS_IsArrayBuffer(o.as_raw()) }) else {
            return sys::napi_generic_failure;
        };
        let off = obj.get("byteOffset", &mut ctx).ok().and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
        let len = obj.get("byteLength", &mut ctx).ok().and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
        if !byte_length.is_null() {
            unsafe { *byte_length = len as usize };
        }
        if !byte_offset.is_null() {
            unsafe { *byte_offset = off as usize };
        }
        if !arraybuffer.is_null() {
            let h = e.handle(JsValue::from(buf_obj.clone()));
            unsafe { *arraybuffer = h };
        }
        if !data.is_null() {
            let mut total: qjs::size_t = 0;
            // SAFETY: `buf_obj` is an ArrayBuffer of this context.
            let base = unsafe { qjs::JS_GetArrayBuffer(ctx.as_raw(), &mut total, buf_obj.as_raw()) };
            unsafe { *data = if base.is_null() { std::ptr::null_mut() } else { base.add(off as usize).cast() } };
        }
        sys::napi_ok
    })
}
