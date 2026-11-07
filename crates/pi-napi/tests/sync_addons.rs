//! Node's `test/js-native-api` synchronous suites, executed on pi-rs against the `pi-napi`
//! implementation of the N-API v8 function table.
//!
//! The fixture addons are compiled once by `crates/pi-napi/tests/build-fixtures.sh` (system
//! `gcc` + Node's headers, no node-gyp); this test only runs the vendored JS suites, so it
//! needs no network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pi_jsrt::{JsRuntime, RuntimeOptions};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js-native-api")
}

/// Run one of Node's JS suites (a CommonJS script, loaded through `require`) and let its
/// assertions decide the outcome.
async fn suite(dir: &str) -> Result<(), String> {
    let root = fixtures();
    let entry = root.join(dir);
    let mut scripts: Vec<String> = std::fs::read_dir(&entry)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(|f| f.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".js"))
        .collect();
    scripts.sort();
    // Node runs each file of a suite; only `test_symbol` ships no `test.js`.
    if scripts.iter().any(|s| s == "test.js") {
        scripts.retain(|s| s == "test.js");
    }
    for script in scripts {
        // Same layout as Node's tree (`tests/common.js` above `tests/js-native-api`), so the
        // suites' `require('../../common')` resolves. Each script gets a fresh runtime, like
        // Node's per-file processes.
        let rt = JsRuntime::new(RuntimeOptions {
            cwd: Some(root.clone()),
            expose_gc: true,
            ..RuntimeOptions::default()
        })
        .map_err(|e| e.to_string())?;
        let specifier = format!("./{dir}/{script}");
        match tokio::time::timeout(Duration::from_secs(120), rt.require(specifier)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(format!("{dir}/{script}: {e:?}\n{}", e.as_js().and_then(|j| j.stack.clone()).unwrap_or_default()))
            }
            Err(_) => return Err(format!("{dir}/{script}: timed out")),
        }
    }
    Ok(())
}

const SUITES: &[&str] = &[
    "2_function_arguments",
    "3_callbacks",
    "4_object_factory",
    "5_function_factory",
    "6_object_wrap",
    "7_factory_wrap",
    "8_passing_wrapped",
    "test_constructor",
    "test_conversions",
    "test_error",
    "test_exception",
    "test_object",
    "test_properties",
    "test_reference",
    "test_string",
    "test_symbol",
    "test_typedarray",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_js_native_api_sync_suites() {
    // `PI_RS_NAPI_SUITE=<dir>` runs one suite (used to bisect a crash).
    let only = std::env::var("PI_RS_NAPI_SUITE").ok();
    let mut failures = Vec::new();
    for dir in SUITES {
        if only.as_deref().is_some_and(|o| o != *dir) {
            continue;
        }
        if let Err(e) = suite(dir).await {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{} suite(s) failed:\n{}", failures.len(), failures.join("\n\n"));
}
