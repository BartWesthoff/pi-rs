//! Process exit helpers: the Rust counterpart of Node's `process.exitCode` / `process.exit()`.
//!
//! TS commands set `process.exitCode` and return; the binary (`src/main.rs`) reads
//! [`exit_code`] after `main()` returns. [`exit`] mirrors `process.exit(code)`: it flushes
//! stdout/stderr and terminates immediately.

use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};

const UNSET: i32 = i32::MIN;
static EXIT_CODE: AtomicI32 = AtomicI32::new(UNSET);

/// `process.exitCode = code`.
pub fn set_exit_code(code: i32) {
    EXIT_CODE.store(code, Ordering::SeqCst);
}

/// `process.exitCode` (`None` when unset).
pub fn exit_code() -> Option<i32> {
    match EXIT_CODE.load(Ordering::SeqCst) {
        UNSET => None,
        code => Some(code),
    }
}

type ExitHook = std::sync::Arc<dyn Fn(i32) + Send + Sync>;

static EXIT_HOOK: parking_lot::RwLock<Option<ExitHook>> = parking_lot::RwLock::new(None);

type GracefulExit = std::sync::Arc<dyn Fn(i32) -> bool + Send + Sync>;

/// Owner of the terminal (interactive mode) that can turn `process.exit()` from extension
/// JavaScript into its graceful shutdown. `false` means the process is no longer running.
static GRACEFUL_EXIT: parking_lot::RwLock<Option<GracefulExit>> = parking_lot::RwLock::new(None);

/// Register (or with `None`, unregister) the graceful shutdown [`request_graceful_exit`] asks for.
pub fn set_graceful_exit(hook: Option<GracefulExit>) {
    *GRACEFUL_EXIT.write() = hook;
}

/// `process.exit(code)` from extension JavaScript: hand the code to the registered owner so it
/// restores the terminal and emits `session_shutdown` first. Without an owner (print and other
/// headless modes) the process exits here. Always reports that the host owns the exit, so
/// JavaScript does not unwind with `ERR_PROCESS_EXIT`.
pub fn request_graceful_exit(code: i32) -> bool {
    match GRACEFUL_EXIT.read().clone() {
        Some(hook) => hook(code),
        None => crate::utils::process_exit::exit(code),
    }
}

/// Test hook: called by [`exit`] before the process exits. A hook that panics (unwinds) keeps
/// the process alive, so tests can drive code paths that end in `process.exit()`.
pub fn set_exit_hook(hook: Option<ExitHook>) {
    *EXIT_HOOK.write() = hook;
}

/// `process.exit(code)`: flush the standard streams and exit.
pub fn exit(code: i32) -> ! {
    let hook = EXIT_HOOK.read().clone();
    if let Some(hook) = hook {
        // A hook that unwinds keeps the process alive; mark the unwind so the uncaught-panic
        // handling (`utils::uncaught`) lets it pass instead of reporting a crash.
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(code))) {
            std::panic::resume_unwind(Box::new(crate::utils::uncaught::ExitHookUnwind(payload)));
        }
    }
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}
