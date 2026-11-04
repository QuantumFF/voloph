fn main() {
    // Link libmpv in-process for embedded playback (ADR 0008). On Linux,
    // libmpv.so.2 lives in the standard library path, so a plain `-lmpv`
    // resolves it. Bundling the shared object beside the app for distribution
    // is a packaging step layered on later. On Windows nothing is needed here:
    // the extern block in src/mpv/mod.rs links libmpv-2.dll via
    // `kind = "raw-dylib"` (ADR 0014), which needs no import library — only the
    // DLL beside the exe at runtime (scripts/fetch-libmpv.sh).
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=dylib=mpv");

    // ONNX Runtime's WebGPU provider links libwebgpu_dawn.so (ADR 0021). ort-sys copies
    // it beside the build output (target/<profile>/ and deps/), and the Linux bundles
    // install it under /usr/lib/Voloph (deb, rpm) or usr/lib (AppImage), so look in
    // all three, relative to the executable.
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/Voloph:$ORIGIN/../lib");

    tauri_build::build()
}
