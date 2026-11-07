//! Wrappers, references and externals: the object-native-pointer bridge of N-API.

use std::ffi::{c_char, c_void};

use pi_quickjs::{JsObject, JsSymbol, JsValue, PropertyKey};

use crate::env::{self, Finalizer, RefRecord, guard_js, value};
use crate::sys::{self, napi_env, napi_finalize, napi_ref, napi_status, napi_value};

/// Is `v` an external created by `napi_create_external`?
pub(crate) fn is_external(env: &env::Env, v: &JsValue) -> bool {
    let Some(obj) = v.as_object() else { return false };
    let Some(sym) = env.symbols.borrow().external.clone() else { return false };
    let mut ctx = env.ctx.clone();
    obj.has_own_property(sym, &mut ctx).unwrap_or(false)
}

/// Register `f` with the env's `FinalizationRegistry` for `object`.
///
/// Returns the token to unregister the entry with, and the id of the finalizer.
fn register_finalizer(env: &mut env::Env, object: &JsObject, fin: Finalizer) -> Option<(JsSymbol, u32)> {
    let registry = env.registry()?;
    let mut ctx = env.ctx.clone();
    let id = env.next_id();
    env.finals.borrow_mut().insert(id, fin);
    let token = JsSymbol::new(Some(pi_quickjs::JsString::from(format!("pi-napi.finalize.{id}"))))?;
    let (Some(register), Some(unregister)) = (
        registry.get("register", &mut ctx).ok().and_then(|v| v.as_function()),
        registry.get("unregister", &mut ctx).ok().and_then(|v| v.as_function()),
    ) else {
        return None;
    };
    let _ = unregister;
    let args = [object.clone().into(), JsValue::from(id), token.clone().into()];
    register.call(&registry.clone().into(), &args, &mut ctx).ok()?;
    Some((token, id))
}

/// The wrap id stored on `obj`, if the object is wrapped.
fn wrap_id(env: &env::Env, obj: &JsObject) -> Option<u32> {
    let sym = env.symbols.borrow().wrap.clone()?;
    let mut ctx = env.ctx.clone();
    let v = obj.get(sym, &mut ctx).ok()?;
    v.as_number().map(|n| n as u32)
}

/// `napi_wrap`
///
/// # Safety
/// `object` must be a live handle; `result` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_wrap(
    env: napi_env,
    object: napi_value,
    native_object: *mut c_void,
    finalize_cb: napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        if wrap_id(e, &obj).is_some() {
            return sys::napi_invalid_arg;
        }
        let sym = env::symbol(e, |s| &mut s.wrap, "pi-napi.wrap");
        let id = e.next_id();
        e.wraps.borrow_mut().insert(id, native_object);
        let mut ctx = e.ctx.clone();
        if obj.set(sym, JsValue::from(id), true, &mut ctx).is_err() {
            return sys::napi_generic_failure;
        }
        if finalize_cb.is_some() || native_object.is_null() {
            let fin = Finalizer { cb: finalize_cb, data: native_object, hint: finalize_hint };
            if register_finalizer(e, &obj, fin).is_none() {
                return sys::napi_generic_failure;
            }
        }
        if !result.is_null() {
            let Some(r) = create_ref(e, &JsValue::from(obj.clone()), 0) else {
                return sys::napi_generic_failure;
            };
            // SAFETY: the caller promises a writable slot.
            unsafe { *result = r };
        }
        sys::napi_ok
    })
}

/// `napi_unwrap`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_unwrap(env: napi_env, object: napi_value, result: *mut *mut c_void) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        let Some(id) = wrap_id(e, &obj) else { return sys::napi_invalid_arg };
        let Some(native) = e.wraps.borrow().get(&id).copied() else { return sys::napi_invalid_arg };
        unsafe { *result = native };
        sys::napi_ok
    })
}

/// `napi_remove_wrap`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_remove_wrap(env: napi_env, object: napi_value, result: *mut *mut c_void) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        let Some(id) = wrap_id(e, &obj) else { return sys::napi_invalid_arg };
        let native = e.wraps.borrow_mut().remove(&id).unwrap_or(std::ptr::null_mut());
        // The wrap finalizer is not called after `napi_remove_wrap`.
        e.finals.borrow_mut().remove(&id);
        let sym = env::symbol(e, |s| &mut s.wrap, "pi-napi.wrap");
        let mut ctx = e.ctx.clone();
        let _ = obj.delete_property(sym, &mut ctx);
        unsafe { *result = native };
        sys::napi_ok
    })
}

/// `napi_add_finalizer`
///
/// # Safety
/// `result` may be null, otherwise writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_add_finalizer(
    env: napi_env,
    js_object: napi_value,
    native_object: *mut c_void,
    finalize_cb: napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    guard_js(env, |e| {
        let Some(v) = (unsafe { value(js_object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        if finalize_cb.is_none() {
            return sys::napi_invalid_arg;
        }
        let fin = Finalizer { cb: finalize_cb, data: native_object, hint: finalize_hint };
        if register_finalizer(e, &obj, fin).is_none() {
            return sys::napi_generic_failure;
        }
        if !result.is_null() {
            let Some(r) = create_ref(e, &JsValue::from(obj.clone()), 0) else {
                return sys::napi_generic_failure;
            };
            unsafe { *result = r };
        }
        sys::napi_ok
    })
}

/// `napi_create_external`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_external(
    env: napi_env,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut ctx = e.ctx.clone();
        let obj = JsObject::with_object_proto(ctx.intrinsics());
        let sym = env::symbol(e, |s| &mut s.external, "pi-napi.external");
        let id = e.next_id();
        e.externals.borrow_mut().insert(id, data);
        if obj.set(sym, JsValue::from(id), true, &mut ctx).is_err() {
            return sys::napi_generic_failure;
        }
        if finalize_cb.is_some() {
            let fin = Finalizer { cb: finalize_cb, data, hint: finalize_hint };
            if register_finalizer(e, &obj, fin).is_none() {
                return sys::napi_generic_failure;
            }
        }
        let h = e.handle(obj.into());
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_get_value_external`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_value_external(env: napi_env, value_: napi_value, result: *mut *mut c_void) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        let Some(sym) = e.symbols.borrow().external.clone() else { return sys::napi_invalid_arg };
        let mut ctx = e.ctx.clone();
        let Some(id) = obj.get(sym, &mut ctx).ok().and_then(|v| v.as_number()).map(|n| n as u32) else {
            return sys::napi_invalid_arg;
        };
        let Some(data) = e.externals.borrow().get(&id).copied() else { return sys::napi_invalid_arg };
        unsafe { *result = data };
        sys::napi_ok
    })
}

/// Create a `napi_ref` for `v`. Returns null on failure.
pub(crate) fn create_ref(env: &mut env::Env, v: &JsValue, count: u32) -> Option<napi_ref> {
    let mut ctx = env.ctx.clone();
    let weak_ctor = ctx.eval(pi_quickjs::Source::from_bytes(b"WeakRef")).ok()?.as_function()?;
    let weak = weak_ctor.construct(std::slice::from_ref(v), None, &mut ctx).ok()?;
    let record = RefRecord { weak, strong: (count > 0).then(|| v.clone()), count };
    let ptr = Box::into_raw(Box::new(record));
    env.refs.borrow_mut().push(ptr);
    Some(ptr.cast())
}

/// The record behind a `napi_ref`.
fn ref_record<'a>(r: napi_ref) -> Option<&'a RefRecord> {
    if r.is_null() { None } else { Some(unsafe { &*r.cast::<RefRecord>() }) }
}

/// `napi_create_reference`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_reference(
    env: napi_env,
    value_: napi_value,
    initial_refcount: u32,
    result: *mut napi_ref,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(value_) }) else { return sys::napi_invalid_arg };
        let v = v.clone();
        let Some(r) = create_ref(e, &v, initial_refcount) else { return sys::napi_generic_failure };
        unsafe { *result = r };
        sys::napi_ok
    })
}

/// `napi_delete_reference`
///
/// # Safety
/// `r` must be a reference from this environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_delete_reference(env: napi_env, r: napi_ref) -> napi_status {
    guard_js(env, |e| {
        if r.is_null() {
            return sys::napi_invalid_arg;
        }
        let mut refs = e.refs.borrow_mut();
        let Some(i) = refs.iter().position(|p| *p == r.cast()) else { return sys::napi_invalid_arg };
        refs.remove(i);
        // SAFETY: the pointer was created by `create_ref` and removed from the list.
        unsafe { drop(Box::from_raw(r.cast::<RefRecord>())) };
        sys::napi_ok
    })
}

/// `napi_reference_ref`
///
/// # Safety
/// `r` must be a reference from this environment; `result` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_reference_ref(env: napi_env, r: napi_ref, result: *mut u32) -> napi_status {
    guard_js(env, |e| {
        let Some(record) = ref_record(r) else { return sys::napi_invalid_arg };
        if record.count == 0 {
            // Promote the weak reference back to a strong one.
            let mut ctx = e.ctx.clone();
            let Some(deref) = record.weak.get("deref", &mut ctx).ok().and_then(|v| v.as_function()) else {
                return sys::napi_generic_failure;
            };
            let Ok(v) = deref.call(&record.weak.clone().into(), &[], &mut ctx) else {
                return env::park(e, pi_quickjs::take_exception(&ctx));
            };
            if v.is_undefined() {
                return sys::napi_generic_failure;
            }
            // SAFETY: the record is live for this call (owned by the environment).
            unsafe { (*r.cast::<RefRecord>()).strong = Some(v) };
        }
        // SAFETY: as above.
        let record = unsafe { &mut *r.cast::<RefRecord>() };
        record.count += 1;
        if !result.is_null() {
            unsafe { *result = record.count };
        }
        sys::napi_ok
    })
}

/// `napi_reference_unref`
///
/// # Safety
/// `r` must be a reference from this environment; `result` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_reference_unref(env: napi_env, r: napi_ref, result: *mut u32) -> napi_status {
    guard_js(env, |e| {
        let Some(record) = ref_record(r) else { return sys::napi_invalid_arg };
        if record.count == 0 {
            return sys::napi_generic_failure;
        }
        // SAFETY: the record is live for this call (owned by the environment).
        let record = unsafe { &mut *r.cast::<RefRecord>() };
        record.count -= 1;
        if record.count == 0 {
            record.strong = None;
        }
        if !result.is_null() {
            unsafe { *result = record.count };
        }
        let _ = e;
        sys::napi_ok
    })
}

/// `napi_get_reference_value`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_reference_value(env: napi_env, r: napi_ref, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(record) = ref_record(r) else { return sys::napi_invalid_arg };
        if let Some(v) = &record.strong {
            let h = e.handle(v.clone());
            unsafe { *result = h };
            return sys::napi_ok;
        }
        let mut ctx = e.ctx.clone();
        let Some(deref) = record.weak.get("deref", &mut ctx).ok().and_then(|v| v.as_function()) else {
            return sys::napi_generic_failure;
        };
        match deref.call(&record.weak.clone().into(), &[], &mut ctx) {
            // Node reports a collected object as a NULL handle, not JS `undefined`.
            Ok(v) if v.is_undefined() => {
                unsafe { *result = std::ptr::null_mut() };
                sys::napi_ok
            }
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_set_instance_data`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_set_instance_data(
    env: napi_env,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    finalize_hint: *mut c_void,
) -> napi_status {
    guard_js(env, |e| {
        e.instance_data.set(data);
        e.instance_finalize.set(finalize_cb);
        e.instance_hint.set(finalize_hint);
        sys::napi_ok
    })
}

/// `napi_get_instance_data`
///
/// # Safety
/// `data` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_instance_data(env: napi_env, data: *mut *mut c_void) -> napi_status {
    guard_js(env, |e| {
        if data.is_null() {
            return sys::napi_invalid_arg;
        }
        unsafe { *data = e.instance_data.get() };
        sys::napi_ok
    })
}

/// `napi_add_env_cleanup_hook`
///
/// # Safety
/// `fun` must be callable with `arg`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_add_env_cleanup_hook(
    env: napi_env,
    fun: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
) -> napi_status {
    guard_js(env, |e| {
        if fun.is_none() {
            return sys::napi_invalid_arg;
        }
        e.add_cleanup_hook(fun, arg);
        sys::napi_ok
    })
}

/// `napi_remove_env_cleanup_hook`
///
/// # Safety
/// `arg` must match a registered hook.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_remove_env_cleanup_hook(
    env: napi_env,
    _fun: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
) -> napi_status {
    guard_js(env, |e| {
        e.remove_cleanup_hook(arg);
        sys::napi_ok
    })
}

/// `node_api_get_module_file_name`
///
/// # Safety
/// `result` must be writable; the returned string is owned by the environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn node_api_get_module_file_name(env: napi_env, result: *mut *const c_char) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        *e.filename_ptr.borrow_mut() = Some(std::ffi::CString::new(e.filename.clone()).unwrap_or_default());
        let p = e.filename_ptr.borrow().as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        unsafe { *result = p };
        sys::napi_ok
    })
}

/// Unused: keeps `PropertyKey` imported for the modules that use it.
#[allow(dead_code)]
fn _keys(_: PropertyKey, _: Vec<u8>) {}
