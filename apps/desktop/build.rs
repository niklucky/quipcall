fn main() {
    // libwebrtc ships Objective-C categories (e.g. NSString+StdString) inside a
    // static library; without -ObjC the linker drops them and WebRTC crashes
    // with "unrecognized selector" on first use.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-ObjC");
    }
}
