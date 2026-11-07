fn main() {
    // The `pi-jsrt-node` harness can load native addons too, so it needs the N-API entry points
    // in its dynamic symbol table (see `crates/pi-coding-agent/build.rs`).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic-symbol=napi_*");
        println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic-symbol=node_api_*");
    }
}
