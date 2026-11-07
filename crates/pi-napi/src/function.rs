//! Functions and constructors: `napi_callback` dispatch, `napi_get_cb_info`, calls, `napi_define_class`.

use std::ffi::{c_char, c_void};

use pi_quickjs::{Context, JsError, JsObject, JsString, JsValue};

use crate::env::{self, guard, guard_js, value};
use crate::object::{callback_function, descriptor_accessors, descriptor_value, key_of};
use crate::sys::{self, napi_callback, napi_callback_info, napi_env, napi_status, napi_value};

/// The state behind a `napi_callback_info`.
pub(crate) struct CallbackInfo {
    /// The `this` of the call (for `new`, a fresh object).
    this: JsValue,
    /// The arguments.
    args: Vec<JsValue>,
    /// The addon data of the function.
    data: *mut c_void,
    /// `new.target` when called as a constructor.
    new_target: Option<JsValue>,
}

/// Run an addon callback (`napi_callback`) and map its result back to QuickJS.
pub(crate) fn invoke(
    env: &mut env::Env,
    cb: unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value,
    this: &JsValue,
    args: &[JsValue],
    data: *mut c_void,
    ctx: &mut Context,
) -> Result<JsValue, JsError> {
    let constructing = pi_quickjs::in_constructor_call();
    env.ctx = ctx.clone();
    let depth = env.scope_depth();
    env.open_scope(false);
    // Constructing a raw function does not create the instance for us (QuickJS hands us
    // `new.target` as `this`): do what V8 does and create it from the constructor's prototype.
    let this_obj = if constructing {
        let mut c = ctx.clone();
        let proto = this.as_object().and_then(|f| f.get("prototype", &mut c).ok()).and_then(|p| p.as_object());
        JsObject::with_proto(proto.as_ref()).into()
    } else {
        this.clone()
    };
    let mut info = CallbackInfo {
        this: this_obj.clone(),
        args: args.to_vec(),
        data,
        new_target: constructing.then(|| this.clone()),
    };
    let env_ptr = (env as *mut env::Env).cast::<c_void>();
    // SAFETY: the callback is a C function from the addon; panics are caught by the caller
    // (`pi_quickjs::fn_call`), and by `catch_unwind` here so our state stays consistent.
    let raw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        cb(env_ptr, (&mut info as *mut CallbackInfo).cast::<c_void>())
    }));
    let raw = match raw {
        Ok(v) => v,
        Err(_) => {
            env.close_scopes_to(depth);
            return Err(JsError::from_native(
                pi_quickjs::JsNativeError::internal().with_message("panic in native addon callback"),
            ));
        }
    };
    // A handle the callback returned lives in a scope we are about to close: duplicate first.
    let returned = unsafe { value(raw) }.cloned();
    let pending = env.has_exception().then(|| env.take_exception());
    env.close_scopes_to(depth);
    if let Some(pending) = pending {
        return Err(JsError::from_opaque(pending));
    }
    let returned = returned.unwrap_or_else(JsValue::undefined);
    if constructing && (returned.is_undefined() || returned.is_null()) {
        return Ok(this_obj);
    }
    Ok(returned)
}

/// `napi_get_cb_info`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_cb_info(
    env: napi_env,
    cbinfo: napi_callback_info,
    argc: *mut usize,
    argv: *mut napi_value,
    this_arg: *mut napi_value,
    data: *mut *mut c_void,
) -> napi_status {
    guard(env, |e| {
        if cbinfo.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: the pointer is the stack frame of the running callback (see `invoke`).
        let info = unsafe { &*cbinfo.cast::<CallbackInfo>() };
        if !argv.is_null() {
            if argc.is_null() {
                return sys::napi_invalid_arg;
            }
            // SAFETY: the caller promises `*argc` writable slots.
            let want = unsafe { *argc };
            let n = want.min(info.args.len());
            for i in 0..n {
                let h = e.handle(info.args[i].clone());
                // SAFETY: `i < want <= *argc` writable slots.
                unsafe { *argv.add(i) = h };
            }
            // Node fills unused slots with undefined; the addon reads them (see `Args`).
            let undefined = e.handle(JsValue::undefined());
            for i in n..want {
                // SAFETY: same range as above.
                unsafe { *argv.add(i) = undefined };
            }
        }
        if !argc.is_null() {
            // SAFETY: checked non-null above.
            unsafe { *argc = info.args.len() };
        }
        if !this_arg.is_null() {
            let h = e.handle(info.this.clone());
            // SAFETY: the caller promises a writable slot.
            unsafe { *this_arg = h };
        }
        if !data.is_null() {
            // SAFETY: the caller promises a writable slot.
            unsafe { *data = info.data };
        }
        sys::napi_ok
    })
}

/// `napi_get_new_target`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_new_target(env: napi_env, cbinfo: napi_callback_info, result: *mut napi_value) -> napi_status {
    guard(env, |e| {
        if cbinfo.is_null() || result.is_null() {
            return sys::napi_invalid_arg;
        }
        // SAFETY: as in `napi_get_cb_info`.
        let info = unsafe { &*cbinfo.cast::<CallbackInfo>() };
        let h = match &info.new_target {
            Some(nt) => e.handle(nt.clone()),
            None => std::ptr::null_mut(),
        };
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_create_function`
///
/// # Safety
/// `utf8name` is a C string (or `length` bytes); `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_create_function(
    env: napi_env,
    utf8name: *const c_char,
    length: usize,
    cb: napi_callback,
    data: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || cb.is_none() {
            return sys::napi_invalid_arg;
        }
        let name = String::from_utf8_lossy(unsafe { env::cstr(utf8name, length) }).into_owned();
        let f = callback_function(e, cb, data, true, Some(JsString::from(name)));
        let h = e.handle(f);
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// `napi_call_function`
///
/// # Safety
/// `argv` must hold `argc` handles; `result` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_call_function(
    env: napi_env,
    recv: napi_value,
    func: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if argc > 0 && argv.is_null() {
            return sys::napi_invalid_arg;
        }
        let (Some(recv), Some(f)) = (unsafe { value(recv) }, unsafe { value(func) }) else {
            return sys::napi_invalid_arg;
        };
        let (recv, f) = (recv.clone(), f.clone());
        let Some(f) = f.as_object().filter(|o| o.is_callable()) else { return sys::napi_function_expected };
        let mut args = Vec::with_capacity(argc);
        for i in 0..argc {
            // SAFETY: the caller promises `argc` handles.
            let Some(v) = (unsafe { value(*argv.add(i)) }) else { return sys::napi_invalid_arg };
            args.push(v.clone());
        }
        let mut ctx = e.ctx.clone();
        match f.call(&recv, &args, &mut ctx) {
            Ok(v) => {
                if !result.is_null() {
                    let h = e.handle(v);
                    unsafe { *result = h };
                }
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_new_instance`
///
/// # Safety
/// `argv` must hold `argc` handles; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_new_instance(
    env: napi_env,
    constructor: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || (argc > 0 && argv.is_null()) {
            return sys::napi_invalid_arg;
        }
        let Some(ctor) = (unsafe { value(constructor) }) else { return sys::napi_invalid_arg };
        let ctor = ctor.clone();
        let Some(ctor) = ctor.as_object().filter(|o| o.is_constructor()) else {
            return sys::napi_function_expected;
        };
        let mut args = Vec::with_capacity(argc);
        for i in 0..argc {
            // SAFETY: the caller promises `argc` handles.
            let Some(v) = (unsafe { value(*argv.add(i)) }) else { return sys::napi_invalid_arg };
            args.push(v.clone());
        }
        let mut ctx = e.ctx.clone();
        match ctor.construct(&args, None, &mut ctx) {
            Ok(v) => {
                let h = e.handle(v.into());
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_define_class`
///
/// # Safety
/// `properties` must hold `property_count` descriptors; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_define_class(
    env: napi_env,
    utf8name: *const c_char,
    length: usize,
    constructor: napi_callback,
    data: *mut c_void,
    property_count: usize,
    properties: *const sys::napi_property_descriptor,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || utf8name.is_null() || constructor.is_none() || (properties.is_null() && property_count > 0) {
            return sys::napi_invalid_arg;
        }
        let name = String::from_utf8_lossy(unsafe { env::cstr(utf8name, length) }).into_owned();
        let ctor = callback_function(e, constructor, data, true, Some(JsString::from(name)));
        let Some(ctor_obj) = ctor.as_object() else { return sys::napi_generic_failure };
        let mut ctx = e.ctx.clone();
        let proto = JsObject::with_object_proto(ctx.intrinsics());
        // prototype.constructor = ctor (writable, not enumerable, configurable)
        let ctor_desc = pi_quickjs::PropertyDescriptor::builder()
            .value(ctor.clone())
            .writable(true)
            .enumerable(false)
            .configurable(true)
            .build();
        if proto.define_property_or_throw(pi_quickjs::PropertyKey::from("constructor"), ctor_desc, &mut ctx).is_err() {
            return sys::napi_generic_failure;
        }
        let proto_desc = pi_quickjs::PropertyDescriptor::builder()
            .value(proto.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false)
            .build();
        if ctor_obj.define_property_or_throw(pi_quickjs::PropertyKey::from("prototype"), proto_desc, &mut ctx).is_err() {
            return sys::napi_generic_failure;
        }
        for i in 0..property_count {
            // SAFETY: the caller promises `property_count` descriptors.
            let d = unsafe { *properties.add(i) };
            let key = if !d.utf8name.is_null() {
                pi_quickjs::PropertyKey::from(JsString::from(String::from_utf8_lossy(unsafe { env::cstr(d.utf8name, usize::MAX) }).into_owned()))
            } else {
                let Some(nv) = (unsafe { value(d.name) }) else { return sys::napi_invalid_arg };
                let Some(k) = key_of(e, nv) else { return sys::napi_name_expected };
                k
            };
            let desc = if d.getter.is_some() || d.setter.is_some() {
                let getter = d.getter.map(|cb| callback_function(e, Some(cb), d.data, false, None)).unwrap_or_else(JsValue::undefined);
                let setter = d.setter.map(|cb| callback_function(e, Some(cb), d.data, false, None)).unwrap_or_else(JsValue::undefined);
                descriptor_accessors(d.attributes, getter, setter)
            } else if d.method.is_some() {
                descriptor_value(d.attributes, callback_function(e, d.method, d.data, false, None))
            } else {
                let Some(v) = (unsafe { value(d.value) }) else { return sys::napi_invalid_arg };
                descriptor_value(d.attributes, v.clone())
            };
            let target = if d.attributes & sys::napi_static != 0 { &ctor_obj } else { &proto };
            if let Err(err) = target.define_property_or_throw(key, desc, &mut ctx) {
                return env::park(e, err);
            }
        }
        let h = e.handle(ctor);
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// Create the native function of a class (used by `napi_define_class`'s fast path).
#[allow(dead_code)]
fn _unused(_: Option<&JsObject>) {}
