//! Acceptance tests for real, unchanged npm addons: the `@napi-rs/keyring` gnu `.node` must load
//! and work, and a V8-ABI addon must be rejected with `ERR_DLOPEN_FAILED`.
//!
//! Both need the pinned fixture packages (`tests/fetch-fixtures.sh`, verified by
//! `tests/fixtures.sha256`) and are therefore ignored by default:
//!
//! ```text
//! crates/pi-napi/tests/fetch-fixtures.sh
//! PI_RS_PKG_FIXTURES="$PWD/target/pi-napi-fixtures" PI_RS_KEYRING_TEST=1 \
//!   cargo test -p pi-napi --test addons -- --ignored
//! ```
//!
//! The keyring test also needs a writable secret store: run it under
//! `dbus-run-session` with `gnome-keyring-daemon --unlock`.

use std::path::PathBuf;

use pi_jsrt::{JsRuntime, RuntimeOptions};
use serde_json::json;

fn fixtures() -> Option<PathBuf> {
    std::env::var_os("PI_RS_PKG_FIXTURES").map(PathBuf::from).filter(|p| p.is_dir())
}

fn runtime(cwd: PathBuf) -> JsRuntime {
    JsRuntime::new(RuntimeOptions { cwd: Some(cwd), expose_gc: true, ..RuntimeOptions::default() })
        .expect("runtime")
}

async fn eval(rt: &JsRuntime, script: &str) -> serde_json::Value {
    rt.eval(script).await.unwrap_or_else(|e| panic!("eval failed: {e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())))
}

/// The real napi-rs keyring addon round-trips a secret.
#[tokio::test]
#[ignore = "needs PI_RS_PKG_FIXTURES and PI_RS_KEYRING_TEST=1 with an unlocked secret store"]
async fn napi_rs_keyring_round_trip() {
    if std::env::var("PI_RS_KEYRING_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set PI_RS_KEYRING_TEST=1");
        return;
    }
    let Some(root) = fixtures() else {
        eprintln!("skipped: set PI_RS_PKG_FIXTURES");
        return;
    };
    let rt = runtime(root.join("keyring"));
    let out = eval(
        &rt,
        "const { Entry } = require('./index.js');
         const entry = new Entry('pi-rs-test', 'a');
         entry.deleteCredential();
         const before = entry.getPassword();
         const set = entry.setPassword('x');
         const got = entry.getPassword();
         const deleted = entry.deleteCredential();
         const after = entry.getPassword();
         // `setPassword` returns undefined (as under Node), which JSON drops.
         ({ before, set: set === undefined, got, deleted, after });",
    )
    .await;
    assert_eq!(out, json!({"before": null, "set": true, "got": "x", "deleted": true, "after": null}));
}

/// A V8-ABI addon is rejected up front, with Node's error code and a N-API explanation.
#[tokio::test]
#[ignore = "needs PI_RS_PKG_FIXTURES (better-sqlite3 prebuilt binary)"]
async fn v8_abi_addon_is_rejected() {
    let Some(root) = fixtures() else {
        eprintln!("skipped: set PI_RS_PKG_FIXTURES");
        return;
    };
    let rt = runtime(root.join("better-sqlite3"));
    let out = eval(
        &rt,
        "try {
           // The prebuilt tarball ships only the binary; requiring it is what `require('better-sqlite3')`
           // ultimately does.
           require('./node_modules/better-sqlite3/build/Release/better_sqlite3.node');
           ({ loaded: true })
         } catch (e) {
           ({ code: e.code, name: e.name, message: e.message })
         }",
    )
    .await;
    assert_eq!(out["code"], "ERR_DLOPEN_FAILED");
    assert_eq!(out["name"], "Error");
    let message = out["message"].as_str().expect("message");
    assert!(
        message.contains("uses the V8 C++ ABI, which pi-rs cannot load"),
        "unexpected message: {message}"
    );
    assert!(!message.contains("UTF-8"), "must not be a decode error: {message}");
}
