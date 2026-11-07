//! Native addons (`.node`): `require()` of a shared library through the N-API layer.
//!
//! An addon is an ELF shared object that imports the `napi_*` C functions and exports an
//! initializer (`napi_register_module_v1`). `pi-napi` implements that C function table, and the
//! `pi` executable exports it (`build.rs`), so an unchanged addon built against Node's headers
//! resolves its imports and runs on pi-quickjs.
//!
//! Addons that import the V8 C++ ABI (`v8::`, `node::`) cannot work here: those symbols do not
//! exist in this process. They are rejected before `dlopen`, so the user sees a clear error
//! instead of a loader failure about an unresolved symbol.

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::Path;

use pi_quickjs::{Context, JsValue};

use crate::error::Error;
use crate::js::JsResult;

/// `RTLD_NOW` on Linux (glibc and musl both use 2; the `libc` crate only exposes it for other
/// platforms).
const RTLD_NOW: c_int = 2;

/// A loaded addon. The library stays mapped for the life of the process (Node does not unload
/// addons either), so values the addon allocated stay valid.
pub(crate) struct Loaded {
    pub(crate) addon: Option<pi_napi::Addon>,
}

impl Loaded {
    /// Run the addon's cleanup hooks and finalizers. The library must still be mapped.
    pub(crate) fn destroy(&mut self) {
        if let Some(addon) = self.addon.take() {
            // SAFETY: the library is still loaded (nothing is ever `dlclose`d) and the addon is
            // destroyed exactly once, by the runtime that loaded it.
            unsafe { addon.destroy() };
        }
    }
}

/// Whether `path` is a native addon.
pub(crate) fn is_addon(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "node")
}

/// Load the addon at `path` and return its exports object.
pub(crate) fn require(ctx: &mut Context, path: &Path) -> JsResult<JsValue> {
    let filename = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| dlopen_failed(ctx, &filename, &e.to_string()))?;
    if let Some(image) = v8_abi_image(&bytes) {
        return Err(dlopen_failed(
            ctx,
            &filename,
            &format!(
                "native addon {name} uses the V8 C++ ABI, which pi-rs cannot load; it imports {image}, \
                 and pi-rs implements Node-API instead — rebuild the addon with a N-API binding \
                 (napi-rs, node-addon-api with NAPI, or a N-API build of the package)",
                name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| filename.clone()),
            ),
        ));
    }

    let c_path = CString::new(filename.as_bytes()).map_err(|_| dlopen_failed(ctx, &filename, "path contains a NUL byte"))?;
    // SAFETY: `c_path` is a valid NUL terminated string.
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: `dlerror` returns either NULL or a valid NUL terminated string.
        let message = unsafe {
            let e = libc::dlerror();
            if e.is_null() { "cannot load shared object".to_string() } else { std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned() }
        };
        return Err(dlopen_failed(ctx, &filename, &message));
    }

    let entry = resolve_symbol(handle, c"napi_register_module_v1");
    let api_version = resolve_symbol(handle, c"node_api_module_get_api_version_v1")
        // SAFETY: the symbol exists and, per Node's headers, is an `int32_t(void)` function.
        .map(|f| unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(f)() })
        .map(|v| v.max(0) as u32);
    // A legacy addon (`NAPI_MODULE_X`/`napi_module_register`) registers its descriptor from a
    // constructor that ran during `dlopen`.
    let module = pi_napi::exports::take_pending_module();
    let entry = match entry {
        Some(f) => Some(unsafe { std::mem::transmute::<*mut c_void, pi_napi::sys::napi_addon_register_func>(f) }),
        // SAFETY: `napi_module_register` stored a static descriptor of the addon we just loaded.
        None => module.map(|m| unsafe { (*m).nm_register_func }),
    };
    let Some(entry) = entry else {
        return Err(dlopen_failed(ctx, &filename, "Module has no declared entry point."));
    };

    let api_version = api_version.unwrap_or_else(|| {
        // SAFETY: the descriptor, when present, is the addon's static `napi_module`.
        module.map_or(8, |m| unsafe { pi_napi::exports::module_api_version(m) })
    });

    // SAFETY: `entry` is the addon's exported initializer from the library we just opened.
    let addon = unsafe { pi_napi::load(ctx, entry, &filename, api_version) }.map_err(|e| {
        // A pending JS exception from the addon's initializer propagates unchanged.
        pi_quickjs::JsError::from_opaque(e)
    })?;
    let exports = addon.exports.clone();
    crate::loader::add_native(ctx, Loaded { addon: Some(addon) });
    Ok(exports.into())
}

/// The DLL name of an imported V8 / Node C++ symbol in `image`, if it imports any.
///
/// The mangled names (`v8::`, `node::`) appear verbatim in the ELF string table, so a byte scan
/// finds them without linking an ELF parser.
/// shortcut: a plain byte scan; upgrade to a `.dynsym` walk if a binary ever embeds those bytes
/// as data and is wrongly rejected.
fn v8_abi_image(image: &[u8]) -> Option<&'static str> {
    const MARKERS: &[(&[u8], &str)] = &[(b"_ZN2v8", "v8::"), (b"_ZN4node", "node::")];
    MARKERS.iter().find(|(needle, _)| find(image, needle).is_some()).map(|(_, name)| *name)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn resolve_symbol(handle: *mut c_void, name: &std::ffi::CStr) -> Option<*mut c_void> {
    // SAFETY: `handle` came from `dlopen`; a miss returns NULL and sets `dlerror` (cleared
    // below so a later `dlopen` failure reports its own reason).
    unsafe {
        let sym = libc::dlsym(handle, name.as_ptr().cast::<c_char>());
        if sym.is_null() {
            libc::dlerror();
        }
        (!sym.is_null()).then_some(sym)
    }
}

/// Node's `ERR_DLOPEN_FAILED`: `Error` carrying the addon path and the reason.
fn dlopen_failed(ctx: &mut Context, filename: &str, reason: &str) -> pi_quickjs::JsError {
    let message = format!("{filename}: {reason}");
    crate::error::to_js(&Error::Js(crate::error::JsException {
        name: "Error".into(),
        message,
        stack: None,
        code: Some("ERR_DLOPEN_FAILED".into()),
    }), ctx)
}
