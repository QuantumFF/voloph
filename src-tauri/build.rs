fn main() {
    // Link libmpv in-process for embedded playback (ADR 0008). The app ships on
    // Linux; on this host libmpv.so.2 lives in the standard library path, so a
    // plain `-lmpv` resolves it. Bundling the shared object beside the app for
    // distribution is a packaging step layered on later.
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=dylib=mpv");

    tauri_build::build()
}
