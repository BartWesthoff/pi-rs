//! The per-addon environment: QuickJS context handle, handle scopes, error bookkeeping and
//! the tables behind wrappers, references, externals and type tags.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::thread::ThreadId;

use pi_quickjs::{Context, JsError, JsNativeError, JsObject, JsSymbol, JsValue};

use crate::sys::{self, napi_env, napi_finalize, napi_handle_scope, napi_status, napi_value};

/// A `napi_handle_scope`: values created while it is open are freed when it closes.
struct Scope {
    /// Index of the first value handle that belongs to this scope.
    base: usize,
    /// Whether `napi_escape_handle` may be used on it.
    escapable: bool,
    /// The value escaped from this scope (kept alive past the close).
    escaped: Option<napi_value>,
}

/// A finalizer registered on an object (`napi_wrap` / `napi_add_finalizer`).
pub(crate) struct Finalizer {
    /// Called with `data` and `hint` when the object is collected.
    pub(crate) cb: napi_finalize,
    /// Addon data.
    pub(crate) data: *mut c_void,
    /// Addon hint.
    pub(crate) hint: *mut c_void,
}

/// A `napi_ref`.
pub(crate) struct RefRecord {
    /// Weak reference to the value (`WeakRef`), so a weak reference does not keep it alive.
    pub(crate) weak: JsObject,
    /// Strong reference, held while the refcount is above zero.
    pub(crate) strong: Option<JsValue>,
    /// `napi_reference_ref` count.
    pub(crate) count: u32,
}

/// Hidden `Symbol` keys used to attach our bookkeeping to plain JS objects.
#[derive(Default)]
pub(crate) struct Symbols {
    /// `obj[sym] = wrap id` (`napi_wrap`).
    pub(crate) wrap: Option<JsSymbol>,
    /// `obj[sym] = external id` (`napi_create_external`).
    pub(crate) external: Option<JsSymbol>,
    /// `obj[sym] = type tag` (`napi_type_tag_object`).
    pub(crate) type_tag: Option<JsSymbol>,
}

/// One `napi_env`: the state of a loaded native addon.
pub(crate) struct Env {
    /// The context the addon's env belongs to.
    pub(crate) ctx: Context,
    /// The thread that owns it (N-API values are thread bound).
    thread: ThreadId,
    /// `NAPI_VERSION` the addon was compiled with.
    #[allow(dead_code)] // kept for `napi_*` version checks
    pub(crate) api_version: u32,
    /// The addon file, for error messages.
    pub(crate) filename: String,
    /// Last returned status.
    last: Cell<napi_status>,
    /// Storage behind `napi_get_last_error_info`.
    info: UnsafeCell<sys::napi_extended_error_info>,
    /// Live value handles (each owns one QuickJS reference).
    values: RefCell<Vec<napi_value>>,
    /// Open handle scopes, innermost last.
    scopes: RefCell<Vec<napi_handle_scope>>,
    /// `napi_set_instance_data` data.
    pub(crate) instance_data: Cell<*mut c_void>,
    /// `napi_set_instance_data` finalizer.
    pub(crate) instance_finalize: Cell<napi_finalize>,
    /// `napi_set_instance_data` hint.
    pub(crate) instance_hint: Cell<*mut c_void>,
    /// `napi_add_env_cleanup_hook` hooks, run at teardown in reverse order.
    cleanup_hooks: RefCell<Vec<(Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void)>>,
    /// Cached hidden-property symbols.
    pub(crate) symbols: RefCell<Symbols>,
    /// Wrapped objects: wrap id → native pointer.
    pub(crate) wraps: RefCell<HashMap<u32, *mut c_void>>,
    /// Pending finalizers: registration id → finalizer.
    pub(crate) finals: RefCell<HashMap<u32, Finalizer>>,
    /// Externals: id → pointer.
    pub(crate) externals: RefCell<HashMap<u32, *mut c_void>>,
    /// Open references.
    pub(crate) refs: RefCell<Vec<*mut RefRecord>>,
    /// Promises waiting for `napi_resolve_deferred` / `napi_reject_deferred`.
    pub(crate) deferreds: RefCell<HashMap<u32, (JsValue, JsValue)>>,
    /// Backing store for `node_api_get_module_file_name`.
    pub(crate) filename_ptr: RefCell<Option<std::ffi::CString>>,
    /// Next id handed out for wraps, externals and finalizers.
    next_id: Cell<u32>,
    /// `FinalizationRegistry` that runs our object finalizers.
    registry: RefCell<Option<JsObject>>,
    /// False once the environment is destroyed (finalizers must not run after that).
    alive: Cell<bool>,
}

impl Env {
    /// Create an environment for `ctx`.
    pub(crate) fn new(ctx: Context, api_version: u32, filename: String) -> Box<Env> {
        let info = sys::napi_extended_error_info {
            error_message: sys::error_message(sys::napi_ok),
            engine_reserved: std::ptr::null_mut(),
            engine_error_code: 0,
            error_code: sys::napi_ok,
        };
        Box::new(Env {
            ctx,
            thread: std::thread::current().id(),
            api_version,
            filename,
            last: Cell::new(sys::napi_ok),
            info: UnsafeCell::new(info),
            values: RefCell::new(Vec::new()),
            scopes: RefCell::new(Vec::new()),
            instance_data: Cell::new(std::ptr::null_mut()),
            instance_finalize: Cell::new(None),
            instance_hint: Cell::new(std::ptr::null_mut()),
            cleanup_hooks: RefCell::new(Vec::new()),
            symbols: RefCell::new(Symbols::default()),
            wraps: RefCell::new(HashMap::new()),
            finals: RefCell::new(HashMap::new()),
            externals: RefCell::new(HashMap::new()),
            refs: RefCell::new(Vec::new()),
            deferreds: RefCell::new(HashMap::new()),
            filename_ptr: RefCell::new(None),
            next_id: Cell::new(1),
            registry: RefCell::new(None),
            alive: Cell::new(true),
        })
    }

    /// Is this environment used from the thread that created it?
    pub(crate) fn on_thread(&self) -> bool {
        self.thread == std::thread::current().id()
    }

    /// The status of the last call.
    pub(crate) fn last_status(&self) -> napi_status {
        self.last.get()
    }

    /// Record the status of the last call.
    pub(crate) fn set_last(&self, status: napi_status) {
        self.last.set(status);
        // SAFETY: only the owning thread touches the cell (enforced by `guard`).
        unsafe {
            let info = &mut *self.info.get();
            info.error_code = status;
            // Node reports a null message for `napi_ok` (its "napi_ok" text is the
            // caller's fallback), which `test_conversions` asserts on.
            info.error_message = if status == sys::napi_ok {
                std::ptr::null()
            } else {
                sys::error_message(status)
            };
        }
    }

    /// The storage behind `napi_get_last_error_info`.
    pub(crate) fn info(&self) -> *const sys::napi_extended_error_info {
        self.info.get()
    }

    /// A new id for wraps, externals and finalizers.
    pub(crate) fn next_id(&self) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        id
    }

    /// Is a JavaScript exception pending?
    pub(crate) fn has_exception(&self) -> bool {
        unsafe { pi_quickjs::rquickjs::qjs::JS_HasException(self.ctx.as_raw()) }
    }

    /// Take the pending exception (leaving none behind).
    pub(crate) fn take_exception(&self) -> JsValue {
        unsafe { JsValue::from_raw(pi_quickjs::rquickjs::qjs::JS_GetException(self.ctx.as_raw())) }
    }

    /// Make `value` the pending exception of the context (takes ownership of it).
    pub(crate) fn throw_value(&self, value: JsValue) {
        unsafe { pi_quickjs::rquickjs::qjs::JS_Throw(self.ctx.as_raw(), value.into_raw()) };
    }

    /// Allocate a value handle in the innermost scope (takes ownership of `v`).
    pub(crate) fn handle(&self, v: JsValue) -> napi_value {
        let p = Box::into_raw(Box::new(v));
        self.values.borrow_mut().push(p.cast());
        p.cast()
    }

    /// Open a handle scope.
    pub(crate) fn open_scope(&self, escapable: bool) -> napi_handle_scope {
        let base = self.values.borrow().len();
        let scope = Box::into_raw(Box::new(Scope { base, escapable, escaped: None }));
        self.scopes.borrow_mut().push(scope.cast());
        scope.cast()
    }

    /// Close a handle scope, freeing the handles created inside it.
    pub(crate) fn close_scope(&self, scope: napi_handle_scope) -> napi_status {
        if self.scopes.borrow().last().copied() != Some(scope) {
            return sys::napi_handle_scope_mismatch;
        }
        self.scopes.borrow_mut().pop();
        // SAFETY: the pointer came from `open_scope` and is no longer in the scope stack.
        let scope = unsafe { Box::from_raw(scope.cast::<Scope>()) };
        let mut values = self.values.borrow_mut();
        let escaped = scope.escaped;
        let mut freed_escaped = false;
        let mut keep = Vec::new();
        for v in values.iter().skip(scope.base) {
            if Some(*v) == escaped {
                keep.push(*v);
                freed_escaped = true;
            } else {
                // SAFETY: every entry above `base` was allocated by `handle`.
                unsafe { drop(Box::from_raw((*v).cast::<JsValue>())) };
            }
        }
        let _ = freed_escaped;
        values.truncate(scope.base);
        for v in keep {
            values.push(v);
        }
        sys::napi_ok
    }

    /// Mark `escapee` as escaped from `scope`.
    pub(crate) fn escape(&self, scope: sys::napi_escapable_handle_scope, escapee: napi_value) -> napi_status {
        if self.scopes.borrow().last().copied() != Some(scope) {
            return sys::napi_handle_scope_mismatch;
        }
        // SAFETY: the scope is open, was allocated by `open_scope`, and only the owning thread
        // reaches here.
        let scope = unsafe { &mut *scope.cast::<Scope>() };
        if !scope.escapable {
            return sys::napi_handle_scope_mismatch;
        }
        if scope.escaped.is_some() {
            return sys::napi_escape_called_twice;
        }
        scope.escaped = Some(escapee);
        sys::napi_ok
    }

    /// Close every scope above `depth` (when a callback returns with scopes still open).
    pub(crate) fn close_scopes_to(&self, depth: usize) {
        while self.scopes.borrow().len() > depth {
            let Some(scope) = self.scopes.borrow().last().copied() else { break };
            let _ = self.close_scope(scope);
        }
    }

    /// Number of open scopes.
    pub(crate) fn scope_depth(&self) -> usize {
        self.scopes.borrow().len()
    }

    /// The environment's `FinalizationRegistry`, created on first use.
    pub(crate) fn registry(&self) -> Option<JsObject> {
        if let Some(r) = self.registry.borrow().as_ref() {
            return Some(r.clone());
        }
        let registry = self.create_registry()?;
        *self.registry.borrow_mut() = Some(registry.clone());
        Some(registry)
    }

    fn create_registry(&self) -> Option<JsObject> {
        let mut ctx = self.ctx.clone();
        let env = (self as *const Env).cast::<c_void>() as usize;
        // The callback receives the registration id and runs the matching finalizer.
        let cb = pi_quickjs::NativeFunction::from_fn(move |_this, args, ctx| {
            if let Some(id) = args.first().and_then(|v| v.as_number())
                // SAFETY: the registry dies with the environment (see `destroy`).
                && let Some(env) = unsafe { (env as *const Env).as_ref() }
            {
                env.run_finalizer(id as u32, ctx);
            }
            Ok(JsValue::undefined())
        })
        .to_js_function(self.ctx.realm());
        let ctor = ctx.eval(pi_quickjs::Source::from_bytes(b"FinalizationRegistry")).ok()?;
        let ctor = ctor.as_function()?;
        ctor.construct(&[cb.into()], None, &mut ctx).ok()
    }

    /// Run the finalizer registered under `id` (if it is still there).
    pub(crate) fn run_finalizer(&self, id: u32, _ctx: &mut Context) {
        if !self.alive.get() {
            return;
        }
        let Some(fin) = self.finals.borrow_mut().remove(&id) else { return };
        self.wraps.borrow_mut().remove(&id);
        if let Some(cb) = fin.cb {
            // SAFETY: N-API finalizers are plain C functions handed to us by the addon; a panic
            // inside one must not unwind into QuickJS.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                cb(self as *const Env as napi_env, fin.data, fin.hint)
            }));
        }
    }

    /// Destroy the environment: run cleanup hooks and release every reference.
    pub(crate) fn destroy(&mut self) {
        self.alive.set(false);
        let hooks: Vec<_> = self.cleanup_hooks.borrow_mut().drain(..).collect();
        for (hook, arg) in hooks.into_iter().rev() {
            if let Some(hook) = hook {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { hook(arg) }));
            }
        }
        if let Some(cb) = self.instance_finalize.get() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                cb(self as *const Env as napi_env, self.instance_data.get(), self.instance_hint.get())
            }));
        }
        for r in self.refs.borrow_mut().drain(..) {
            // SAFETY: every entry was created by `refs::create`.
            unsafe { drop(Box::from_raw(r)) };
        }
        self.finals.borrow_mut().clear();
        self.wraps.borrow_mut().clear();
        self.externals.borrow_mut().clear();
        self.deferreds.borrow_mut().clear();
        for v in self.values.borrow_mut().drain(..) {
            // SAFETY: every entry was allocated by `handle`.
            unsafe { drop(Box::from_raw(v.cast::<JsValue>())) };
        }
    }

    /// Register a cleanup hook.
    pub(crate) fn add_cleanup_hook(&self, hook: Option<unsafe extern "C" fn(*mut c_void)>, arg: *mut c_void) {
        self.cleanup_hooks.borrow_mut().push((hook, arg));
    }

    /// Remove a cleanup hook registered with `arg`.
    pub(crate) fn remove_cleanup_hook(&self, arg: *mut c_void) {
        let mut hooks = self.cleanup_hooks.borrow_mut();
        if let Some(i) = hooks.iter().rposition(|(_, a)| *a == arg) {
            hooks.remove(i);
        }
    }
}

/// Borrow a value handle.
///
/// # Safety
/// `v` must be a handle created by this crate and not yet freed.
pub(crate) unsafe fn value<'a>(v: napi_value) -> Option<&'a JsValue> {
    if v.is_null() { None } else { Some(unsafe { &*v.cast::<JsValue>() }) }
}

/// Read a C string argument (`NAPI_AUTO_LENGTH` means "up to the NUL").
///
/// # Safety
/// `s` must be NUL terminated when `len` is `usize::MAX`.
pub(crate) unsafe fn cstr<'a>(s: *const c_char, len: usize) -> &'a [u8] {
    if s.is_null() {
        return &[];
    }
    // SAFETY: the addon passes either a NUL terminated string or `len` readable bytes.
    unsafe {
        if len == usize::MAX {
            CStr::from_ptr(s).to_bytes()
        } else {
            std::slice::from_raw_parts(s.cast::<u8>(), len)
        }
    }
}

fn note(env: &Env, status: napi_status) -> napi_status {
    env.set_last(status);
    status
}

/// Run `f` with the environment, catching panics so they never cross the FFI boundary.
pub(crate) fn guard<F>(env: napi_env, f: F) -> napi_status
where
    F: FnOnce(&mut Env) -> napi_status,
{
    if env.is_null() {
        return sys::napi_invalid_arg;
    }
    // SAFETY: `env` came from `create` and stays alive until the addon is unloaded.
    let e = unsafe { &mut *env.cast::<Env>() };
    if !e.on_thread() {
        return note(e, sys::napi_generic_failure);
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(e))) {
        Ok(status) => note(e, status),
        Err(_) => note(e, sys::napi_generic_failure),
    }
}

/// Like [`guard`], but also rejects calls made while an exception is pending (Node's
/// `NAPI_PREAMBLE`).
pub(crate) fn guard_js<F>(env: napi_env, f: F) -> napi_status
where
    F: FnOnce(&mut Env) -> napi_status,
{
    guard(env, |e| {
        if e.has_exception() {
            return sys::napi_pending_exception;
        }
        f(e)
    })
}

/// Store the thrown error as the pending exception and report `napi_pending_exception`.
pub(crate) fn park(env: &Env, err: JsError) -> napi_status {
    let value = match err.into_opaque(&mut env.ctx.clone()) {
        Ok(v) => v,
        Err(e) => {
            let mut ctx = env.ctx.clone();
            JsNativeError::error().with_message(e.to_string()).to_opaque(&mut ctx).into()
        }
    };
    env.throw_value(value);
    sys::napi_pending_exception
}

/// Create the environment of a newly loaded addon.
pub(crate) fn create(ctx: Context, api_version: u32, filename: &str) -> Box<Env> {
    Env::new(ctx, api_version, filename.to_string())
}

/// Destroy an environment created by [`create`].
///
/// # Safety
/// `env` must come from [`create`] and must not be used afterwards.
pub(crate) unsafe fn destroy(env: napi_env) {
    if env.is_null() {
        return;
    }
    // SAFETY: the caller guarantees sole ownership (the loader drops the addon once).
    let mut env = unsafe { Box::from_raw(env.cast::<Env>()) };
    env.destroy();
}

/// The hidden symbol for `slot` (created on first use).
pub(crate) fn symbol(env: &Env, slot: fn(&mut Symbols) -> &mut Option<JsSymbol>, name: &str) -> JsSymbol {
    let mut symbols = env.symbols.borrow_mut();
    let slot = slot(&mut symbols);
    if let Some(s) = slot {
        return s.clone();
    }
    let s = match JsSymbol::new(Some(pi_quickjs::JsString::from(name))) {
        Some(s) => s,
        None => unreachable!("symbol allocation cannot fail in practice"),
    };
    *slot = Some(s.clone());
    s
}
