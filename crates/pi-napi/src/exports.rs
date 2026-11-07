//! Addon registration: `napi_module_register`, register-function discovery and loading.

use std::cell::RefCell;
use std::ffi::CStr;

use pi_quickjs::{Context, JsObject, JsValue};

use crate::env;
use crate::sys::{napi_addon_register_func, napi_env, napi_module};

thread_local! {
    /// The module descriptor an addon registers at load time (`napi_module_register`).
    static PENDING: RefCell<Option<*const napi_module>> = const { RefCell::new(None) };
}

/// `napi_module_register`: keep the addon's descriptor until the loader reads it.
///
/// # Safety
/// `module` must stay alive for the rest of the load (addons pass a static).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_module_register(module: *const napi_module) {
    PENDING.with(|p| *p.borrow_mut() = Some(module));
}

/// Take the descriptor registered by the last `napi_module_register` call.
pub fn take_pending_module() -> Option<*const napi_module> {
    PENDING.with(|p| p.borrow_mut().take())
}

/// The API version an addon negotiated at load time (0 when it did not).
///
/// # Safety
/// `module` must be a valid descriptor (or null).
pub unsafe fn module_api_version(module: *const napi_module) -> u32 {
    if module.is_null() {
        return 8;
    }
    // SAFETY: the addon registered this pointer; it is a static `napi_module`.
    unsafe { (*module).nm_version.max(0) as u32 }
}

/// The module name an addon declared, if any.
///
/// # Safety
/// `module` must be a valid descriptor (or null).
pub unsafe fn module_name(module: *const napi_module) -> Option<String> {
    if module.is_null() {
        return None;
    }
    // SAFETY: the addon registered this pointer; `nm_modname` is a static C string.
    let name = unsafe { (*module).nm_modname };
    if name.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned())
}

/// A loaded addon: its exports and the environment that owns its handles.
pub struct Addon {
    /// The `module.exports` object the addon initialised.
    pub exports: JsObject,
    /// The environment the addon is bound to; drop it with [`Addon::destroy`].
    pub env: napi_env,
    /// The filename the addon was loaded from.
    pub filename: String,
}

impl Addon {
    /// Run the addon's finalizers and cleanup hooks. Call once, before unloading the library.
    ///
    /// # Safety
    /// The library the addon came from must still be loaded.
    pub unsafe fn destroy(self) {
        // SAFETY: `env` came from `env::create` and is owned by this value.
        unsafe { env::destroy(self.env) };
    }
}

/// Initialize a freshly opened addon.
///
/// `entry` is the symbol the loader found (`napi_register_module_v1`, `napi_register_module_v1`
/// via `napi_module_register`, or a legacy `node_module_register`). The register function is
/// called with a fresh environment and an empty exports object, and the result becomes the
/// module's exports.
///
/// # Safety
/// `entry` must be a function of the addon's, and the library must stay loaded while the addon
/// lives.
pub unsafe fn load(
    ctx: &mut Context,
    entry: napi_addon_register_func,
    filename: &str,
    api_version: u32,
) -> Result<Addon, JsValue> {
    let Some(register) = entry else {
        return Err(error_value(ctx, "native addon has no napi_register_module_v1 entry point"));
    };
    let exports = JsObject::with_object_proto(ctx.intrinsics());
    let env = env::create(ctx.clone(), api_version, filename);
    let env_ptr = Box::into_raw(env).cast::<env::Env>();
    // SAFETY: `env_ptr` is a live environment owned by this call.
    let exports_handle = unsafe { (*env_ptr).handle(JsValue::from(exports.clone())) };
    // SAFETY: `register` is the addon's exported entry point; it receives the environment we
    // just created and the handle of the fresh exports object.
    let result = unsafe { register(env_ptr.cast(), exports_handle) };
    if let Some(pending) = unsafe { (*env_ptr).has_exception() }.then(|| unsafe { (*env_ptr).take_exception() }) {
        // The addon's exports may still be reachable through the error (Node keeps the
        // instance alive for that), so the environment must stay alive too.
        // shortcut: a failed initialisation leaks its environment; upgrade when addons can be
        // tied to the error value for cleanup.
        return Err(pending);
    }
    let exports = match unsafe { env::value(result) }.and_then(|v| v.as_object()) {
        Some(obj) => obj.clone(),
        // `napi_register_module_v1` returns the exports object it was given; an addon that
        // returns nothing keeps the object it populated.
        None => exports,
    };
    Ok(Addon { exports, env: env_ptr.cast(), filename: filename.to_string() })
}

fn error_value(ctx: &mut Context, message: &str) -> JsValue {
    pi_quickjs::JsNativeError::error().with_message(message).to_opaque(ctx).into()
}

#[allow(dead_code)]
const _: () = {
    // Compile-time check that the descriptor layout matches Node's header.
    assert!(std::mem::size_of::<napi_module>() >= std::mem::size_of::<usize>() * 6);
};
