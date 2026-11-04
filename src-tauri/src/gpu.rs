//! Where the ONNX models run (ADR 0021): the per-OS GPU execution providers, the one
//! process-wide ONNX Runtime environment, and a clean exit after WebGPU was used.
//!
//! Linux runs the shuttle tracker on ONNX Runtime's WebGPU provider (Dawn over the
//! system's Vulkan driver), so a GPU needs no CUDA install and the download grows by
//! one 13 MB library rather than ~1 GB of CUDA runtime. The person detector and pose
//! model stay on CPU on Linux: they are cheap at the rates the app runs them, and
//! WebGPU has already shown one silent wrong-answer bug (a 27-input-channel
//! convolution, worked around in the TrackNet export), so it is used only where it is
//! checked — see `tracknet::ShuttleTracker::load`'s self-check. Windows keeps DirectML
//! for every model.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Once;

use ort::execution_providers::ExecutionProviderDispatch;
use ort::session::builder::SessionBuilder;
use ort::session::Session;

/// A session builder, after the process-wide ONNX Runtime environment exists — the
/// only way the app gets one. A GPU provider registered on a builder *before* the
/// environment exists cannot reach ONNX Runtime's default logger, and WebGPU then
/// fails to register — silently, leaving that session on CPU (`ort` creates the
/// environment lazily, after the first builder's providers).
pub(crate) fn session_builder() -> ort::Result<SessionBuilder> {
    init_environment();
    Session::builder()
}

/// Load `model` into a session on `providers`, most-preferred first (`ort` skips any
/// that cannot register and falls back to CPU; none = CPU), with `intra_threads`
/// inference threads. Errors only when the model itself cannot be loaded, never for
/// a missing GPU.
pub(crate) fn build_session(
    model: &Path,
    providers: Vec<ExecutionProviderDispatch>,
    intra_threads: usize,
) -> Result<Session, String> {
    session_builder()
        .map_err(|e| format!("ort session builder: {e}"))?
        .with_execution_providers(providers)
        .map_err(|e| format!("ort execution providers: {e}"))?
        .with_intra_threads(intra_threads)
        .map_err(|e| format!("ort intra threads: {e}"))?
        .commit_from_file(model)
        .map_err(|e| format!("ort could not load {}: {e}", model.display()))
}

/// Create the process-wide ONNX Runtime environment, once, before any session (see
/// [`session_builder`]).
fn init_environment() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(e) = ort::init().with_name("voloph").commit() {
            log::warn!("onnx runtime: environment init failed: {e}");
        }
    });
}

/// Resolve a vendored ONNX model file by name. Tauri copies `bundle.resources` into a
/// `resources/` folder beside the app executable in a release build, and the same
/// layout is used in `tauri dev`; the dev CLIs run from `target/<profile>/` where the
/// model is likewise expected at `resources/models/…` or `models/…`. We probe the
/// candidate layouts in order and return the first that exists, so both dev and release
/// resolve without a code change. `label` names the model in the not-found error, which
/// lists every tried path. Shared by the detector, the pose pass and the
/// shuttle tracker.
pub(crate) fn vendored_model_file(model_file: &str, label: &str) -> Result<PathBuf, String> {
    let mut tried: Vec<PathBuf> = Vec::new();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()));
    if let Some(dir) = &exe_dir {
        // Release/dev bundle layout: resources next to the executable.
        tried.push(dir.join("resources").join("models").join(model_file));
        tried.push(dir.join("models").join(model_file));
    }
    // Source-tree layout: running the dev CLI from the crate root / target dir.
    tried.push(PathBuf::from("models").join(model_file));
    tried.push(PathBuf::from("src-tauri").join("models").join(model_file));
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        tried.push(PathBuf::from(manifest).join("models").join(model_file));
    }
    for p in &tried {
        if p.exists() {
            return Ok(p.clone());
        }
    }
    Err(format!(
        "vendored {label} model not found; looked in: {}",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// GPU providers for the person detector and the pose model, most-preferred first;
/// `ort` skips any that cannot register and falls back to CPU (ADR 0015 — identical
/// output, only wall-clock differs). Empty on Linux: those models stay on CPU there.
pub(crate) fn general_providers() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "windows")]
    {
        vec![ort::execution_providers::DirectMLExecutionProvider::default().build()]
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

/// GPU providers for the shuttle tracker (TrackNet + InpaintNet): WebGPU on Linux,
/// DirectML on Windows. The tracker verifies the GPU's answers against CPU once per
/// process before trusting them.
pub(crate) fn shuttle_providers() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "linux")]
    {
        vec![ort::execution_providers::WebGPUExecutionProvider::default().build()]
    }
    #[cfg(target_os = "windows")]
    {
        vec![ort::execution_providers::DirectMLExecutionProvider::default().build()]
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// Whether a provider name (as `tracknet::probe_execution_provider` reports it) is
/// WebGPU.
pub(crate) fn is_webgpu(provider: &str) -> bool {
    provider.starts_with("WebGpu")
}

/// Whether a provider name (as `tracknet::probe_execution_provider` reports it, or a
/// `"CPU (...)"` fallback reason) means the sessions run on the CPU.
pub(crate) fn is_cpu(provider: &str) -> bool {
    provider.starts_with("CPU")
}

static WEBGPU_USED: AtomicBool = AtomicBool::new(false);
static SESSIONS_LIVE: AtomicUsize = AtomicUsize::new(0);

/// Held by everything that owns an ONNX session (detector, pose model, shuttle
/// tracker): while one is alive [`exit`] must not release the runtime underneath it.
/// `webgpu` marks that the owner's sessions run on WebGPU.
pub(crate) struct SessionInUse(());

impl SessionInUse {
    pub(crate) fn acquire(webgpu: bool) -> SessionInUse {
        if webgpu {
            WEBGPU_USED.store(true, Ordering::SeqCst);
        }
        SESSIONS_LIVE.fetch_add(1, Ordering::SeqCst);
        SessionInUse(())
    }
}

impl Drop for SessionInUse {
    fn drop(&mut self) {
        SESSIONS_LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// End the process with `code`. Without WebGPU this is [`std::process::exit`]. After
/// WebGPU was used, ONNX Runtime's static teardown at `exit` destroys cached GPU
/// pipelines after their device is gone and segfaults — unless the environment is
/// released first, which runs that cleanup in order. So: with no ONNX session alive,
/// release the environment and exit normally; with one still alive (quitting mid
/// analysis — releasing under a running session is unsafe), flush and `_exit`, skipping
/// static destructors. Call it last: no ONNX session may be used afterwards.
pub fn exit(code: i32) -> ! {
    if WEBGPU_USED.load(Ordering::SeqCst) {
        if SESSIONS_LIVE.load(Ordering::SeqCst) == 0 {
            if let Ok(env) = ort::environment::get_environment() {
                use ort::AsPointer;
                // SAFETY: the environment is only ever released here, on the way out,
                // and no ONNX session is alive; nothing uses ort after this call.
                unsafe { (ort::api().ReleaseEnv)(env.ptr().cast_mut()) };
            }
        } else {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            log::logger().flush();
            // SAFETY: `_exit` ends the process at once; nothing runs after it.
            // `libc` is a Linux-only dependency, and only Linux runs WebGPU.
            #[cfg(target_os = "linux")]
            unsafe {
                libc::_exit(code)
            };
        }
    }
    std::process::exit(code)
}
