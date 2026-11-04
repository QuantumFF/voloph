//! The `teacher-run` dev CLI (issue #105): the bundled teacher on a shuttle-track CSV
//! and pose arrays — the parity check against scripts/silver-ctx.py. All the work lives
//! in `tauri_native_lib::eval::teacher_run_cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match tauri_native_lib::eval::teacher_run_cli(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("teacher-run: {e}");
            1
        }
    };
    // Through gpu::exit: a plain exit segfaults in ONNX Runtime's teardown after WebGPU.
    tauri_native_lib::gpu::exit(code);
}
