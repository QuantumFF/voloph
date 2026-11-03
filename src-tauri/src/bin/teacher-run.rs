//! The `teacher-run` dev CLI (issue #105): the bundled teacher on a shuttle-track CSV
//! and pose arrays — the parity check against scripts/silver-ctx.py. All the work lives
//! in `tauri_native_lib::eval::teacher_run_cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::eval::teacher_run_cli(args) {
        eprintln!("teacher-run: {e}");
        std::process::exit(1);
    }
}
