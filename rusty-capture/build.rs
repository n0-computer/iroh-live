fn main() {
    // On Windows, native capture backends (PipeWire, V4L2, ScreenCaptureKit)
    // are not available. Enable the `capture_fallback` cfg so that xcap and
    // nokhwa are used when their features are enabled.
    //
    // On Linux and macOS, the native backends are preferred and the fallback
    // crates are only used if explicitly requested via feature flags.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        println!("cargo:rustc-cfg=capture_fallback");
    }
}
