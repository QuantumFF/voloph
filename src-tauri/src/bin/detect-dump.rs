//! The `detect-dump` dev CLI (ADR 0015 Stage 2, issue #83): run the occupancy person
//! detector on one real recording and print its detection track — per-sample box
//! counts, positions, and sizes — plus the wall-clock vs real-time ratio. A thin entry
//! point; all the work lives in `tauri_native_lib::detect`.
//!
//! It resolves the bundled ffmpeg/ffprobe sidecars and the vendored ONNX model the same
//! way the app does — relative to the running binary — so run it from the same
//! `target/` folder a `tauri dev`/build populated, or point `--file` at a media file
//! directly. ONNX Runtime is fetched by the `ort` build; the detector probes for a GPU
//! and silently falls back to CPU.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::detect::run(args) {
        eprintln!("detect-dump: {e}");
        std::process::exit(1);
    }
}
