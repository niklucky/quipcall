fn main() {
    // ggml's Metal code checks `@available(macOS …)`, which calls
    // `__isPlatformVersionAtLeast` from clang's runtime. Clang adds that
    // library itself, but rustc links with -nodefaultlibs, so ask for it.
    // (Plain -l finds the static archive; it propagates to every binary.)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::process::Command::new("xcrun")
        .args(["clang", "--print-runtime-dir"])
        .output()
        .expect("xcrun clang: install the Xcode command line tools");
    let dir = String::from_utf8(out.stdout).expect("runtime dir");
    println!("cargo:rustc-link-search=native={}", dir.trim());
    println!("cargo:rustc-link-lib=dylib=clang_rt.osx");
}
