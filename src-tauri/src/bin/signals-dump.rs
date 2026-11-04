//! The `signals-dump` dev CLI (issue #105): motion, audio and person-detector signals
//! for one recording, through the app's own extractors, as JSON — the student
//! segmenter's inputs. All the work lives in `tauri_native_lib::eval::signals_dump_cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = tauri_native_lib::eval::signals_dump_cli(args) {
        eprintln!("signals-dump: {e}");
        std::process::exit(1);
    }
}
