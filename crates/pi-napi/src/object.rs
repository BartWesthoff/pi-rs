//! Object, property and array operations of the synchronous Node-API surface.

use std::ffi::{c_char, c_void};

use pi_quickjs::{Context, JsObject, JsString, JsValue, PropertyDescriptor, PropertyKey};

use crate::env::{self, guard_js, value};
use crate::sys::{self, napi_env, napi_status, napi_value};

/// Coerce a `napi_value` used as a property key (string, symbol or number).
pub(crate) fn key_of(env: &mut env::Env, v: &JsValue) -> Option<PropertyKey> {
    if let Some(s) = v.as_string() {
        return Some(s.into());
    }
    if let Some(sym) = v.as_symbol() {
        return Some(sym.into());
    }
    if v.is_number() {
        // Node applies ToPropertyKey: numeric keys become their string form.
        let mut ctx = env.ctx.clone();
        return v.to_string(&mut ctx).ok().map(PropertyKey::from);
    }
    None
}

/// The object behind a `napi_value` handle.
pub(crate) fn object_of(env: &mut env::Env, v: napi_value) -> Result<JsObject, napi_status> {
    let Some(v) = (unsafe { value(v) }) else { return Err(sys::napi_invalid_arg) };
    if let Some(o) = v.as_object() {
        return Ok(o);
    }
    // Node coerces with ToObject for property access.
    let mut ctx = env.ctx.clone();
    v.to_object(&mut ctx).map_err(|err| env::park(env, err))
}

/// `napi_get_property`
///
/// # Safety
/// `key` and `result` follow the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_property(env: napi_env, object: napi_value, key: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(kv) = (unsafe { value(key) }) else { return sys::napi_invalid_arg };
        let Some(k) = key_of(e, kv) else { return sys::napi_name_expected };
        let mut ctx = e.ctx.clone();
        match obj.get(k, &mut ctx) {
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_set_property`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_set_property(env: napi_env, object: napi_value, key: napi_value, val: napi_value) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let (Some(kv), Some(v)) = (unsafe { value(key) }, unsafe { value(val) }) else {
            return sys::napi_invalid_arg;
        };
        let Some(k) = key_of(e, kv) else { return sys::napi_name_expected };
        let v = v.clone();
        let mut ctx = e.ctx.clone();
        match obj.set(k, v, true, &mut ctx) {
            Ok(_) => sys::napi_ok,
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_has_property`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_has_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(kv) = (unsafe { value(key) }) else { return sys::napi_invalid_arg };
        let Some(k) = key_of(e, kv) else { return sys::napi_name_expected };
        let mut ctx = e.ctx.clone();
        match obj.has_property(k, &mut ctx) {
            Ok(b) => {
                unsafe { *result = b };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_has_own_property`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_has_own_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(kv) = (unsafe { value(key) }) else { return sys::napi_invalid_arg };
        // Node checks `IsName()`: only strings and symbols qualify (numbers do not).
        if !(kv.is_string() || kv.is_symbol()) {
            return sys::napi_name_expected;
        }
        let Some(k) = key_of(e, kv) else { return sys::napi_name_expected };
        let mut ctx = e.ctx.clone();
        match obj.has_own_property(k, &mut ctx) {
            Ok(b) => {
                unsafe { *result = b };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_delete_property`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_delete_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(kv) = (unsafe { value(key) }) else { return sys::napi_invalid_arg };
        let Some(k) = key_of(e, kv) else { return sys::napi_name_expected };
        let mut ctx = e.ctx.clone();
        match obj.delete_property(k, &mut ctx) {
            Ok(b) => {
                if !result.is_null() {
                    unsafe { *result = b };
                }
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_get_named_property`
///
/// # Safety
/// `utf8name` must be a valid C string; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || utf8name.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let name = String::from_utf8_lossy(unsafe { env::cstr(utf8name, usize::MAX) }).into_owned();
        let mut ctx = e.ctx.clone();
        match obj.get(name, &mut ctx) {
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_set_named_property`
///
/// # Safety
/// `utf8name` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_set_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, val: napi_value) -> napi_status {
    guard_js(env, |e| {
        if utf8name.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(v) = (unsafe { value(val) }) else { return sys::napi_invalid_arg };
        let name = String::from_utf8_lossy(unsafe { env::cstr(utf8name, usize::MAX) }).into_owned();
        let v = v.clone();
        let mut ctx = e.ctx.clone();
        match obj.set(name, v, true, &mut ctx) {
            Ok(_) => sys::napi_ok,
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_has_named_property`
///
/// # Safety
/// `utf8name` must be a valid C string; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_has_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() || utf8name.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let name = String::from_utf8_lossy(unsafe { env::cstr(utf8name, usize::MAX) }).into_owned();
        let mut ctx = e.ctx.clone();
        match obj.has_property(name, &mut ctx) {
            Ok(b) => {
                unsafe { *result = b };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_get_element`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_element(env: napi_env, object: napi_value, index: u32, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        match obj.get(index, &mut ctx) {
            Ok(v) => {
                let h = e.handle(v);
                unsafe { *result = h };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_set_element`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_set_element(env: napi_env, object: napi_value, index: u32, val: napi_value) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let Some(v) = (unsafe { value(val) }) else { return sys::napi_invalid_arg };
        let v = v.clone();
        let mut ctx = e.ctx.clone();
        match obj.set(index, v, true, &mut ctx) {
            Ok(_) => sys::napi_ok,
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_has_element`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_has_element(env: napi_env, object: napi_value, index: u32, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        match obj.has_property(index, &mut ctx) {
            Ok(b) => {
                unsafe { *result = b };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_delete_element`
///
/// # Safety
/// `result` may be null, otherwise writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_delete_element(env: napi_env, object: napi_value, index: u32, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        match obj.delete_property(index, &mut ctx) {
            Ok(b) => {
                if !result.is_null() {
                    unsafe { *result = b };
                }
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_get_array_length`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_array_length(env: napi_env, object: napi_value, result: *mut u32) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_array_expected };
        if !obj.is_array() {
            return sys::napi_array_expected;
        }
        let mut ctx = e.ctx.clone();
        match obj.get("length", &mut ctx).and_then(|l| l.to_number(&mut ctx)) {
            Ok(n) => {
                unsafe { *result = n as u32 };
                sys::napi_ok
            }
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_is_array`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_is_array(env: napi_env, value_: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        let v = unsafe { value(value_) };
        let (Some(v), false) = (v, result.is_null()) else {
            return sys::napi_invalid_arg;
        };
        let _ = e;
        unsafe { *result = v.as_object().is_some_and(|o| o.is_array()) };
        sys::napi_ok
    })
}

/// `napi_get_prototype`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_prototype(env: napi_env, object: napi_value, result: *mut napi_value) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let proto = obj.prototype().map(|p| p.into()).unwrap_or_else(JsValue::null);
        unsafe { *result = e.handle(proto) };
        sys::napi_ok
    })
}

/// `napi_instanceof`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_instanceof(env: napi_env, object: napi_value, constructor: napi_value, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let (Some(obj), Some(ctor)) = (unsafe { value(object) }, unsafe { value(constructor) }) else {
            return sys::napi_invalid_arg;
        };
        let (obj, ctor) = (obj.clone(), ctor.clone());
        if ctor.as_object().is_none_or(|o| !o.is_callable()) {
            return sys::napi_function_expected;
        }
        let ctx = e.ctx.clone();
        // SAFETY: both values belong to `ctx`.
        let r = unsafe { pi_quickjs::rquickjs::qjs::JS_IsInstanceOf(ctx.as_raw(), obj.as_raw(), ctor.as_raw()) };
        if r < 0 {
            return env::park(e, pi_quickjs::take_exception(&ctx));
        }
        unsafe { *result = r != 0 };
        sys::napi_ok
    })
}

/// `napi_object_freeze`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_object_freeze(env: napi_env, object: napi_value) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        match obj.freeze(&mut ctx) {
            Ok(()) => sys::napi_ok,
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_object_seal`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_object_seal(env: napi_env, object: napi_value) -> napi_status {
    guard_js(env, |e| {
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        // SAFETY: `as_raw` is an object of this context.
        let ok = unsafe { pi_quickjs::rquickjs::qjs::JS_SealObject(e.ctx.as_raw(), obj.as_raw()) } >= 0;
        if ok { sys::napi_ok } else { sys::napi_generic_failure }
    })
}

/// `napi_get_property_names`: enumerable own and inherited string keys.
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_property_names(env: napi_env, object: napi_value, result: *mut napi_value) -> napi_status {
    unsafe { napi_get_all_property_names(env, object, sys::napi_key_include_prototypes, sys::napi_key_enumerable | sys::napi_key_skip_symbols, sys::napi_key_numbers_to_strings, result) }
}

/// `napi_get_all_property_names`
///
/// # Safety
/// `result` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_get_all_property_names(
    env: napi_env,
    object: napi_value,
    key_mode: sys::napi_key_collection_mode,
    key_filter: sys::napi_key_filter,
    _key_conversion: sys::napi_key_conversion,
    result: *mut napi_value,
) -> napi_status {
    guard_js(env, |e| {
        if result.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        let raw = obj.as_raw();
        let mut flags = 0u32;
        if key_filter & sys::napi_key_skip_strings == 0 {
            flags |= pi_quickjs::rquickjs::qjs::JS_GPN_STRING_MASK;
        }
        if key_filter & sys::napi_key_skip_symbols == 0 {
            flags |= pi_quickjs::rquickjs::qjs::JS_GPN_SYMBOL_MASK;
        }
        let own_only = key_mode == sys::napi_key_own_only;
        let mut props: *mut pi_quickjs::rquickjs::qjs::JSPropertyEnum = std::ptr::null_mut();
        let mut count: u32 = 0;
        // SAFETY: the out pointers are valid; the returned array is freed below.
        let n = unsafe {
            pi_quickjs::rquickjs::qjs::JS_GetOwnPropertyNames(ctx.as_raw(), &mut props, &mut count, raw, flags as i32)
        };
        if n < 0 {
            return env::park(e, pi_quickjs::take_exception(&ctx));
        }
        let mut keys: Vec<JsValue> = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: `props` holds `count` entries.
            let atom = unsafe { (*props.add(i)).atom };
            // SAFETY: `atom` is a live atom of this context.
            let k = unsafe { JsValue::from_raw(pi_quickjs::rquickjs::qjs::JS_AtomToValue(ctx.as_raw(), atom)) };
            keys.push(k);
        }
        // SAFETY: the array came from `JS_GetOwnPropertyNames`.
        unsafe { pi_quickjs::rquickjs::qjs::JS_FreePropertyEnum(ctx.as_raw(), props, count) };
        if !own_only {
            let mut proto = obj.prototype();
            while let Some(p) = proto {
                let mut pprops: *mut pi_quickjs::rquickjs::qjs::JSPropertyEnum = std::ptr::null_mut();
                let mut pcount: u32 = 0;
                // SAFETY: as above.
                let n = unsafe {
                    pi_quickjs::rquickjs::qjs::JS_GetOwnPropertyNames(ctx.as_raw(), &mut pprops, &mut pcount, p.as_raw(), flags as i32)
                };
                if n < 0 {
                    break;
                }
                for i in 0..pcount as usize {
                    // SAFETY: `pprops` holds `pcount` entries.
                    let atom = unsafe { (*pprops.add(i)).atom };
                    // SAFETY: live atom of this context.
                    let k = unsafe { JsValue::from_raw(pi_quickjs::rquickjs::qjs::JS_AtomToValue(ctx.as_raw(), atom)) };
                    let dup = keys.iter().any(|existing| existing.strict_equals(&k));
                    if !dup {
                        keys.push(k);
                    }
                }
                // SAFETY: the array came from `JS_GetOwnPropertyNames`.
                unsafe { pi_quickjs::rquickjs::qjs::JS_FreePropertyEnum(ctx.as_raw(), pprops, pcount) };
                proto = p.prototype();
            }
        }
        let keep_symbols = key_filter & sys::napi_key_skip_symbols == 0;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if k.is_symbol() && !keep_symbols {
                continue;
            }
            // V8 filters on the property found along the prototype chain, so look it up there.
            let Some(desc) = lookup_descriptor(&obj, &k, &mut ctx) else { continue };
            if key_filter & sys::napi_key_enumerable != 0 && !desc.enumerable().unwrap_or(false) {
                continue;
            }
            if key_filter & sys::napi_key_writable != 0 && !desc.writable().unwrap_or(false) {
                continue;
            }
            if key_filter & sys::napi_key_configurable != 0 && !desc.configurable().unwrap_or(false) {
                continue;
            }
            out.push(k);
        }
        let arr = pi_quickjs::JsArray::new(&mut ctx);
        for k in out {
            let _ = arr.push(k, &mut ctx);
        }
        let h = e.handle(arr.into());
        unsafe { *result = h };
        sys::napi_ok
    })
}

/// The descriptor of `key` on `obj` or, failing that, on the nearest prototype that has it.
fn lookup_descriptor(obj: &JsObject, key: &JsValue, ctx: &mut Context) -> Option<PropertyDescriptor> {
    let key = if let Some(s) = key.as_symbol() {
        PropertyKey::from(s)
    } else {
        PropertyKey::from(key.as_string()?)
    };
    let mut current = Some(obj.clone());
    while let Some(o) = current {
        if let Ok(Some(desc)) = o.get_own_property(key.clone(), ctx) {
            return Some(desc);
        }
        current = o.prototype();
    }
    None
}

/// `napi_type_tag_object`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_type_tag_object(env: napi_env, object: napi_value, type_tag: *const sys::napi_type_tag) -> napi_status {
    guard_js(env, |e| {
        if type_tag.is_null() {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        // SAFETY: the caller passes a readable `napi_type_tag`.
        let tag = unsafe { *type_tag };
        let sym = env::symbol(e, |s| &mut s.type_tag, "pi-napi.type_tag");
        let mut ctx = e.ctx.clone();
        if matches!(obj.has_own_property(sym.clone(), &mut ctx), Ok(true)) {
            return sys::napi_invalid_arg;
        }
        let text = format!("{:016x}{:016x}", tag.lower, tag.upper);
        match obj.set(sym, JsString::from(text), true, &mut ctx) {
            Ok(_) => sys::napi_ok,
            Err(err) => env::park(e, err),
        }
    })
}

/// `napi_check_object_type_tag`
///
/// # Safety
/// Follows the N-API contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_check_object_type_tag(env: napi_env, object: napi_value, type_tag: *const sys::napi_type_tag, result: *mut bool) -> napi_status {
    guard_js(env, |e| {
        if type_tag.is_null() || result.is_null() {
            return sys::napi_invalid_arg;
        }
        let Some(v) = (unsafe { value(object) }) else { return sys::napi_invalid_arg };
        let Some(obj) = v.as_object() else { return sys::napi_invalid_arg };
        // SAFETY: the caller passes a readable `napi_type_tag`.
        let tag = unsafe { *type_tag };
        let sym = env::symbol(e, |s| &mut s.type_tag, "pi-napi.type_tag");
        let mut ctx = e.ctx.clone();
        let stored = obj.get(sym, &mut ctx).ok().and_then(|v| v.as_string()).map(|s| s.to_std_string_lossy()).unwrap_or_default();
        let want = format!("{:016x}{:016x}", tag.lower, tag.upper);
        unsafe { *result = stored == want };
        sys::napi_ok
    })
}

/// A property descriptor with an explicit value or accessors.
pub(crate) fn descriptor_value(attrs: sys::napi_property_attributes, value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor::builder()
        .value(value)
        .writable(attrs & sys::napi_writable != 0)
        .enumerable(attrs & sys::napi_enumerable != 0)
        .configurable(attrs & sys::napi_configurable != 0)
        .build()
}

/// A property descriptor with accessors.
pub(crate) fn descriptor_accessors(
    attrs: sys::napi_property_attributes,
    get: JsValue,
    set: JsValue,
) -> PropertyDescriptor {
    PropertyDescriptor::builder()
        .get(get)
        .set(set)
        .enumerable(attrs & sys::napi_enumerable != 0)
        .configurable(attrs & sys::napi_configurable != 0)
        .build()
}


/// `napi_define_properties`
///
/// # Safety
/// `properties` must point to `property_count` descriptors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_define_properties(
    env: napi_env,
    object: napi_value,
    property_count: usize,
    properties: *const sys::napi_property_descriptor,
) -> napi_status {
    guard_js(env, |e| {
        if properties.is_null() && property_count > 0 {
            return sys::napi_invalid_arg;
        }
        let obj = match object_of(e, object) {
            Ok(o) => o,
            Err(status) => return status,
        };
        let mut ctx = e.ctx.clone();
        for i in 0..property_count {
            // SAFETY: the caller passes `property_count` descriptors.
            let d = unsafe { *properties.add(i) };
            let key = if !d.utf8name.is_null() {
                PropertyKey::from(JsString::from(String::from_utf8_lossy(unsafe { env::cstr(d.utf8name, usize::MAX) }).into_owned()))
            } else {
                let Some(name) = (unsafe { value(d.name) }) else { return sys::napi_invalid_arg };
                let Some(k) = key_of(e, name) else { return sys::napi_name_expected };
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
            match obj.define_property_or_throw(key, desc, &mut ctx) {
                Ok(true) => {}
                Ok(false) => return sys::napi_generic_failure,
                Err(err) => return env::park(e, err),
            }
        }
        sys::napi_ok
    })
}

/// Build a JS function that calls back into the addon.
pub(crate) fn callback_function(
    env: &mut env::Env,
    cb: sys::napi_callback,
    data: *mut c_void,
    constructor: bool,
    name: Option<JsString>,
) -> JsValue {
    let Some(cb) = cb else { return JsValue::undefined() };
    let env_ptr = (env as *mut env::Env).cast::<c_void>();
    let f = pi_quickjs::NativeFunction::from_fn(move |this: &JsValue, args: &[JsValue], ctx: &mut Context| {
        // SAFETY: the environment outlives every function created from it (the loader keeps it
        // alive for the lifetime of the module).
        let env = unsafe { &mut *env_ptr.cast::<env::Env>() };
        crate::function::invoke(env, cb, this, args, data, ctx)
    });
    let realm = env.ctx.realm().clone();
    let mut b = pi_quickjs::FunctionObjectBuilder::new(&realm, f).length(0).constructor(constructor);
    if let Some(n) = name {
        b = b.name(n);
    }
    b.build().into()
}
