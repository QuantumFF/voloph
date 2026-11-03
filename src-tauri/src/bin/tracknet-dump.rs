//! The `tracknet-dump` dev CLI (issue #105): run the app's TrackNetV3 port over one
//! whole recording and write the shuttle track in `teacher-tracknet.py`'s CSV format,
//! for parity checks against the offline run. All the work lives in
//! `tauri_native_lib::tracknet::dump_cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::tracknet::dump_cli(args) {
        eprintln!("tracknet-dump: {e}");
        std::process::exit(1);
    }
}
