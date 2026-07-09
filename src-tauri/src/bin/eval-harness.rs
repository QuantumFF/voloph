//! The `eval-harness` dev CLI (ADR 0015): re-run the current segmenter against the
//! hand-corrected gold timelines in the app's DB and print the acceptance-bar
//! numbers. A thin entry point — all the work lives in `tauri_native_lib::eval`.
//!
//! It resolves the bundled ffmpeg/ffprobe sidecars the same way the app does — from
//! the directory of the running binary — so run it from the same `target/` folder a
//! `tauri dev`/build populated with the sidecars, or put ffmpeg on PATH beside it.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::eval::run(args) {
        eprintln!("eval-harness: {e}");
        std::process::exit(1);
    }
}
