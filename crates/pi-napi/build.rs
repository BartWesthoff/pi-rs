fn main() {
    // A native addon resolves `napi_*` from the dynamic symbol table of the process that
    // `dlopen`s it, so the test binaries must export the table too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-tests=-Wl,--export-dynamic-symbol=napi_*");
        println!("cargo:rustc-link-arg-tests=-Wl,--export-dynamic-symbol=node_api_*");
    }
}
