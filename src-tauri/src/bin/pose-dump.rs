//! The `pose-dump` dev CLI (issue #105): run the app's own person detector + RTMPose-t
//! over one whole recording and write the pose track as JSON, so the rally teacher's
//! pose features are trained on the stack the app ships. All the work lives in
//! `tauri_native_lib::pose::dump_cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::pose::dump_cli(args) {
        eprintln!("pose-dump: {e}");
        std::process::exit(1);
    }
}
