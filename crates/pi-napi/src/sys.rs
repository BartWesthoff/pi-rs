//! Node-API (N-API) C types and status codes, as declared by `js_native_api_types.h`.
//!
//! Everything here is `repr(C)` and `pub` so the `#[no_mangle]` functions in the crate's
//! modules can use it. The layout must match Node's headers exactly: addons are compiled
//! against them and call into these functions through the ProcessGlobal symbols.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void};

/// Opaque environment handle: a pointer to the [`crate::env::Env`] of one loaded addon.
pub type napi_env = *mut c_void;
/// Function-table entry for the same environment (the "basic" variant of the header).
pub type node_api_basic_env = *mut c_void;
/// A JavaScript value handle.
pub type napi_value = *mut c_void;
/// A reference to a JavaScript value.
pub type napi_ref = *mut c_void;
/// A handle scope.
pub type napi_handle_scope = *mut c_void;
/// An escapable handle scope.
pub type napi_escapable_handle_scope = *mut c_void;
/// Callback info passed to a `napi_callback`.
pub type napi_callback_info = *mut c_void;
/// A promise deferred.
pub type napi_deferred = *mut c_void;
/// An async context.
pub type napi_async_context = *mut c_void;
/// An async work item.
pub type napi_async_work = *mut c_void;
/// A thread-safe function.
pub type napi_callback_scope = *mut c_void;

/// `napi_threadsafe_function`
pub type napi_threadsafe_function = *mut c_void;

/// `napi_status`
pub type napi_status = i32;

/// `napi_ok`
pub const napi_ok: napi_status = 0;
/// `napi_invalid_arg`
pub const napi_invalid_arg: napi_status = 1;
/// `napi_object_expected`
pub const napi_object_expected: napi_status = 2;
/// `napi_string_expected`
pub const napi_string_expected: napi_status = 3;
/// `napi_name_expected`
pub const napi_name_expected: napi_status = 4;
/// `napi_function_expected`
pub const napi_function_expected: napi_status = 5;
/// `napi_number_expected`
pub const napi_number_expected: napi_status = 6;
/// `napi_boolean_expected`
pub const napi_boolean_expected: napi_status = 7;
/// `napi_array_expected`
pub const napi_array_expected: napi_status = 8;
/// `napi_generic_failure`
pub const napi_generic_failure: napi_status = 9;
/// `napi_pending_exception`
pub const napi_pending_exception: napi_status = 10;
/// `napi_cancelled`
pub const napi_cancelled: napi_status = 11;
/// `napi_escape_called_twice`
pub const napi_escape_called_twice: napi_status = 12;
/// `napi_handle_scope_mismatch`
pub const napi_handle_scope_mismatch: napi_status = 13;
/// `napi_callback_scope_mismatch`
pub const napi_callback_scope_mismatch: napi_status = 14;
/// `napi_queue_full`
pub const napi_queue_full: napi_status = 15;
/// `napi_closing`
pub const napi_closing: napi_status = 16;
/// `napi_bigint_expected`
pub const napi_bigint_expected: napi_status = 17;
/// `napi_date_expected`
pub const napi_date_expected: napi_status = 18;
/// `napi_arraybuffer_expected`
pub const napi_arraybuffer_expected: napi_status = 19;
/// `napi_detachable_arraybuffer_expected`
pub const napi_detachable_arraybuffer_expected: napi_status = 20;
/// `napi_would_deadlock`
pub const napi_would_deadlock: napi_status = 21;
/// `napi_no_external_buffers_allowed`
pub const napi_no_external_buffers_allowed: napi_status = 22;
/// `napi_cannot_run_js`
pub const napi_cannot_run_js: napi_status = 23;

/// `napi_valuetype`
pub type napi_valuetype = i32;
/// `napi_valuetype::napi_undefined`
pub const napi_undefined: napi_valuetype = 0;
/// `napi_valuetype::napi_null`
pub const napi_null: napi_valuetype = 1;
/// `napi_valuetype::napi_boolean`
pub const napi_boolean: napi_valuetype = 2;
/// `napi_valuetype::napi_number`
pub const napi_number: napi_valuetype = 3;
/// `napi_valuetype::napi_string`
pub const napi_string: napi_valuetype = 4;
/// `napi_valuetype::napi_symbol`
pub const napi_symbol: napi_valuetype = 5;
/// `napi_valuetype::napi_object`
pub const napi_object: napi_valuetype = 6;
/// `napi_valuetype::napi_function`
pub const napi_function: napi_valuetype = 7;
/// `napi_valuetype::napi_external`
pub const napi_external: napi_valuetype = 8;
/// `napi_valuetype::napi_bigint`
pub const napi_bigint: napi_valuetype = 9;

/// `napi_typedarray_type`
pub type napi_typedarray_type = i32;
/// `napi_int8_array`
pub const napi_int8_array: napi_typedarray_type = 0;
/// `napi_uint8_array`
pub const napi_uint8_array: napi_typedarray_type = 1;
/// `napi_uint8_clamped_array`
pub const napi_uint8_clamped_array: napi_typedarray_type = 2;
/// `napi_int16_array`
pub const napi_int16_array: napi_typedarray_type = 3;
/// `napi_uint16_array`
pub const napi_uint16_array: napi_typedarray_type = 4;
/// `napi_int32_array`
pub const napi_int32_array: napi_typedarray_type = 5;
/// `napi_uint32_array`
pub const napi_uint32_array: napi_typedarray_type = 6;
/// `napi_float32_array`
pub const napi_float32_array: napi_typedarray_type = 7;
/// `napi_float64_array`
pub const napi_float64_array: napi_typedarray_type = 8;
/// `napi_bigint64_array`
pub const napi_bigint64_array: napi_typedarray_type = 9;
/// `napi_biguint64_array`
pub const napi_biguint64_array: napi_typedarray_type = 10;
/// `napi_float16_array`
pub const napi_float16_array: napi_typedarray_type = 11;

/// `napi_property_attributes`
pub type napi_property_attributes = i32;
/// `napi_default`
pub const napi_default: napi_property_attributes = 0;
/// `napi_writable`
pub const napi_writable: napi_property_attributes = 1 << 0;
/// `napi_enumerable`
pub const napi_enumerable: napi_property_attributes = 1 << 1;
/// `napi_configurable`
pub const napi_configurable: napi_property_attributes = 1 << 2;
/// `napi_static`
pub const napi_static: napi_property_attributes = 1 << 10;
/// `napi_default_method`
pub const napi_default_method: napi_property_attributes = napi_writable | napi_configurable;
/// `napi_default_jsproperty`
pub const napi_default_jsproperty: napi_property_attributes = napi_writable | napi_enumerable | napi_configurable;

/// `napi_key_collection_mode`
pub type napi_key_collection_mode = i32;
/// `napi_key_include_prototypes`
pub const napi_key_include_prototypes: napi_key_collection_mode = 0;
/// `napi_key_own_only`
pub const napi_key_own_only: napi_key_collection_mode = 1;

/// `napi_key_filter`
pub type napi_key_filter = i32;
/// `napi_key_all_properties`
pub const napi_key_all_properties: napi_key_filter = 0;
/// `napi_key_writable`
pub const napi_key_writable: napi_key_filter = 1;
/// `napi_key_enumerable`
pub const napi_key_enumerable: napi_key_filter = 1 << 1;
/// `napi_key_configurable`
pub const napi_key_configurable: napi_key_filter = 1 << 2;
/// `napi_key_skip_strings`
pub const napi_key_skip_strings: napi_key_filter = 1 << 3;
/// `napi_key_skip_symbols`
pub const napi_key_skip_symbols: napi_key_filter = 1 << 4;

/// `napi_key_conversion`
pub type napi_key_conversion = i32;
/// `napi_key_keep_numbers`
pub const napi_key_keep_numbers: napi_key_conversion = 0;
/// `napi_key_numbers_to_strings`
pub const napi_key_numbers_to_strings: napi_key_conversion = 1;

/// `napi_callback`: a function an addon exports to JavaScript.
pub type napi_callback = Option<unsafe extern "C" fn(env: napi_env, info: napi_callback_info) -> napi_value>;
/// `napi_finalize`
pub type napi_finalize = Option<unsafe extern "C" fn(env: node_api_basic_env, data: *mut c_void, hint: *mut c_void)>;
/// `napi_addon_register_func`
pub type napi_addon_register_func = Option<unsafe extern "C" fn(env: napi_env, exports: napi_value) -> napi_value>;
/// `napi_async_execute_callback`
pub type napi_async_execute_callback = Option<unsafe extern "C" fn(env: napi_env, data: *mut c_void)>;
/// `napi_async_complete_callback`
pub type napi_async_complete_callback = Option<unsafe extern "C" fn(env: napi_env, status: napi_status, data: *mut c_void)>;
/// `napi_threadsafe_function_call_js`
pub type napi_threadsafe_function_call_js =
    Option<unsafe extern "C" fn(env: napi_env, js_callback: napi_value, context: *mut c_void, data: *mut c_void)>;
/// `napi_cleanup_hook`
pub type napi_cleanup_hook = Option<unsafe extern "C" fn(arg: *mut c_void)>;
/// `napi_async_cleanup_hook`
pub type napi_async_cleanup_hook = Option<unsafe extern "C" fn(handle: *mut c_void, arg: *mut c_void)>;
/// `node_api_async_cleanup_hook_handle`
pub type napi_async_cleanup_hook_handle = *mut c_void;
/// `uv_loop_s`
pub type uv_loop_s = c_void;

/// `napi_property_descriptor`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct napi_property_descriptor {
    /// UTF-8 property name (or null when `name` is used).
    pub utf8name: *const c_char,
    /// Property name as a `napi_value`.
    pub name: napi_value,
    /// Method callback.
    pub method: napi_callback,
    /// Getter callback.
    pub getter: napi_callback,
    /// Setter callback.
    pub setter: napi_callback,
    /// Data value.
    pub value: napi_value,
    /// Attributes.
    pub attributes: napi_property_attributes,
    /// Data passed to the callbacks.
    pub data: *mut c_void,
}

/// `napi_extended_error_info`
#[repr(C)]
pub struct napi_extended_error_info {
    /// Human readable message for `error_code`.
    pub error_message: *const c_char,
    /// Reserved for the engine.
    pub engine_reserved: *mut c_void,
    /// Reserved for the engine.
    pub engine_error_code: u32,
    /// The last status of this environment.
    pub error_code: napi_status,
}

/// `napi_type_tag`
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct napi_type_tag {
    /// Lower half of the tag.
    pub lower: u64,
    /// Upper half of the tag.
    pub upper: u64,
}

/// `napi_module`
#[repr(C)]
pub struct napi_module {
    /// `NAPI_MODULE_VERSION` of the addon.
    pub nm_version: i32,
    /// Module flags.
    pub nm_flags: u32,
    /// The addon file name.
    pub nm_filename: *const c_char,
    /// Registration function.
    pub nm_register_func: napi_addon_register_func,
    /// Module name.
    pub nm_modname: *const c_char,
    /// Private data of the addon.
    pub nm_priv: *mut c_void,
    /// Reserved.
    pub reserved: [*mut c_void; 4],
}

/// `napi_node_version`
#[repr(C)]
pub struct napi_node_version {
    /// Node major version.
    pub major: u32,
    /// Node minor version.
    pub minor: u32,
    /// Node patch version.
    pub patch: u32,
    /// Release name (`"node"`).
    pub release: *const c_char,
}

/// `napi_async_work_callback` data holder used by our stand-in async work implementation.
#[repr(C)]
pub struct napi_async_work_data {
    /// Resource object.
    pub resource: napi_value,
    /// Resource name.
    pub resource_name: napi_value,
    /// Executes off the JS thread (unsupported here).
    pub execute: napi_async_execute_callback,
    /// Runs on the JS thread.
    pub complete: napi_async_complete_callback,
    /// User data.
    pub data: *mut c_void,
}

/// Messages matching `napi_status`, indexed by status code (Node's `error_messages[]`).
pub(crate) fn error_message(status: napi_status) -> *const c_char {
    const MESSAGES: [&std::ffi::CStr; 24] = [
        c"",
        c"Invalid argument",
        c"An object was expected",
        c"A string was expected",
        c"A string or symbol was expected",
        c"A function was expected",
        c"A number was expected",
        c"A boolean was expected",
        c"An array was expected",
        c"Unknown failure",
        c"An exception is pending",
        c"The async work item was cancelled",
        c"napi_escape_handle already called on scope",
        c"Invalid handle scope usage",
        c"Invalid callback scope usage",
        c"Thread-safe function queue is full",
        c"Thread-safe function handle is closing",
        c"A bigint was expected",
        c"A date was expected",
        c"An arraybuffer was expected",
        c"A detachable arraybuffer was expected",
        c"Main thread would deadlock",
        c"External buffers are not allowed",
        c"Cannot run JavaScript",
    ];
    let i = usize::try_from(status).unwrap_or(0);
    MESSAGES.get(i).copied().unwrap_or(c"Unknown failure").as_ptr()
}
