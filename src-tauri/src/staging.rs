//! Bounded local staging cache for analysis on network-declared mounts (ADR 0011).
//!
//! Analysis makes at least two full reads per recording (`media.rs`: separate
//! ffmpeg runs for 16 kHz audio and 5 fps frames), plus the probe. On a shared
//! library whose mount is declared **network**, streaming the file across the
//! mount that many times is wasteful. Instead each file is **staged**: copied
//! once into a bounded local staging area, every pass runs against the staged
//! copy, then the copy is **evicted**. Copy-ahead (copy the next file while the
//! current one is analyzed) keeps the pipeline from stalling between files, and a
//! size **budget** caps how much local disk staging may use at once — a session
//! far larger than the budget still processes within it.
//!
//! Local-declared mounts and the local library never stage (`media.rs` runs in
//! place against the original); staging is a network-mount concern only.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// How many bytes of staged copies may exist at once. A network-mount recording
/// is copied whole before analysis, and copy-ahead holds a second one, so the
/// budget must fit the current plus the next file to keep the pipeline moving.
///
/// ponytail: fixed default, overridable at launch via `VOLOPH_STAGING_BUDGET`
/// (bytes). Promote to a per-device setting if users need to tune it from the UI.
const DEFAULT_BUDGET_BYTES: u64 = 8 * 1024 * 1024 * 1024; // 8 GiB

/// The staging area's budget in bytes, from `VOLOPH_STAGING_BUDGET` when set and
/// parseable, else the default.
pub fn budget_bytes() -> u64 {
    std::env::var("VOLOPH_STAGING_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_BUDGET_BYTES)
}

/// Delete every leftover staged copy in `dir` (and the dir itself if empty of
/// anything but our copies). Run at launch so an app quit mid-copy or
/// mid-analysis leaves no orphaned staged files behind — the staging area holds
/// only in-flight copies, never durable state, so wiping it on start is safe.
pub fn clean(dir: &Path) {
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            log::warn!("staging: could not clear stale staging area {dir:?}: {e}");
        }
    }
}

/// A staged copy of a recording. Dropping it **evicts** the copy (deletes the
/// local file), so a copy never outlives the analysis that needed it — the drop
/// runs on the happy path and on early return / panic alike.
pub struct Staged {
    path: PathBuf,
    bytes: u64,
}

impl Staged {
    /// The local path analysis should read instead of the network original.
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("staging: could not evict {:?}: {e}", self.path);
            }
        }
    }
}

/// Copy `src` (a network original) into `dir` under a unique name, returning a
/// [`Staged`] guard. The copy is the single network crossing for this recording;
/// every analysis pass then reads the returned local path.
fn copy_in(dir: &Path, src: &Path) -> Result<Staged, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create staging area: {e}"))?;
    let name = src
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("staged"));
    // A recording's library-relative name is unique, but two different
    // recordings can share a file name; prefix with the process-unique counter so
    // staged copies never collide.
    let unique = format!("{}-{}", next_seq(), name.to_string_lossy());
    let dest = dir.join(unique);
    let bytes = std::fs::copy(src, &dest)
        .map_err(|e| format!("could not stage {}: {e}", src.display()))?;
    Ok(Staged { path: dest, bytes })
}

/// Process-unique sequence for staged file names.
fn next_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// The size of `src`, or 0 when it cannot be read (the copy itself will then
/// surface the real error).
fn size_of(src: &Path) -> u64 {
    std::fs::metadata(src).map(|m| m.len()).unwrap_or(0)
}

/// A running copy-ahead: a background thread staging one recording while another
/// is analyzed.
struct Prefetch {
    handle: JoinHandle<Result<Staged, String>>,
}

/// A bounded, copy-ahead staging cache. Stage recordings one at a time via
/// [`stage`](Self::stage); while the caller analyzes the returned copy, the cache
/// has already begun copying the *next* one — provided both fit within the budget.
pub struct StagingCache {
    dir: PathBuf,
    budget: u64,
    /// An in-flight copy-ahead of the next path, if one was started.
    pending: Option<(PathBuf, Prefetch)>,
}

impl StagingCache {
    pub fn new(dir: PathBuf, budget: u64) -> Self {
        Self {
            dir,
            budget,
            pending: None,
        }
    }

    /// Stage `path` and return the local copy, then kick off copy-ahead of
    /// `next` (the path the caller will ask for after this one) when it fits in
    /// the budget alongside the current copy. The returned [`Staged`] must be held
    /// for the whole analysis and dropped to evict.
    ///
    /// Copy-ahead never lets total staged bytes exceed the budget: the next file
    /// is prefetched only if it fits alongside the current one; otherwise it is
    /// copied on demand when the caller reaches it.
    pub fn stage(&mut self, path: &Path, next: Option<&Path>) -> Result<Staged, String> {
        let staged = match self.take_pending(path) {
            Some(result) => result?,
            None => copy_in(&self.dir, path)?,
        };
        self.start_prefetch(staged.bytes(), next);
        Ok(staged)
    }

    /// If a prefetch for `path` is in flight, join it and return its result;
    /// otherwise `None`.
    fn take_pending(&mut self, path: &Path) -> Option<Result<Staged, String>> {
        match self.pending.take() {
            Some((p, prefetch)) if p == path => {
                Some(prefetch.handle.join().unwrap_or_else(|_| {
                    Err("staging: copy-ahead thread panicked".to_string())
                }))
            }
            // Prefetch was for a different path (queue changed) — discard it. Its
            // Staged drops here, evicting the wasted copy.
            other => {
                if let Some((_, prefetch)) = other {
                    if let Ok(Ok(staged)) = prefetch.handle.join() {
                        drop(staged);
                    }
                }
                None
            }
        }
    }

    /// Start copying `next` in the background when it fits in the remaining budget
    /// alongside the `current` staged copy. Oversized-pair case: skip copy-ahead,
    /// the caller stages `next` on demand (still correct, just no overlap).
    fn start_prefetch(&mut self, current: u64, next: Option<&Path>) {
        let Some(next) = next else { return };
        let next_bytes = size_of(next);
        if current + next_bytes > self.budget {
            return; // would exceed the budget — analyze without copy-ahead
        }
        let dir = self.dir.clone();
        let src = next.to_path_buf();
        let handle = std::thread::spawn(move || copy_in(&dir, &src));
        self.pending = Some((next.to_path_buf(), Prefetch { handle }));
    }
}

impl Drop for StagingCache {
    fn drop(&mut self) {
        // Evict any in-flight prefetch so quitting mid-batch leaves nothing behind
        // (launch cleanup also covers this, but tidy up eagerly).
        if let Some((_, prefetch)) = self.pending.take() {
            if let Ok(Ok(staged)) = prefetch.handle.join() {
                drop(staged);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, size: usize) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&vec![7u8; size]).unwrap();
        p
    }

    /// A staged copy exists during use and is evicted when dropped — no orphan
    /// left behind.
    #[test]
    fn staged_copy_is_evicted_on_drop() {
        let tmp = std::env::temp_dir().join(format!("voloph-stage-test-{}", next_seq()));
        let src_dir = tmp.join("src");
        let stage_dir = tmp.join("stage");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = write_file(&src_dir, "a.mp4", 128);

        let staged_path;
        {
            let s = copy_in(&stage_dir, &src).unwrap();
            staged_path = s.path().to_path_buf();
            assert!(staged_path.exists(), "staged copy should exist during use");
            assert_eq!(s.bytes(), 128);
        }
        assert!(!staged_path.exists(), "staged copy should be evicted on drop");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Copy-ahead overlaps: after staging the first file, the second is already
    /// (or soon) staged and served without a fresh copy, and total staged bytes
    /// never exceed the budget.
    #[test]
    fn copy_ahead_prefetches_next_within_budget() {
        let tmp = std::env::temp_dir().join(format!("voloph-stage-test-{}", next_seq()));
        let src_dir = tmp.join("src");
        let stage_dir = tmp.join("stage");
        std::fs::create_dir_all(&src_dir).unwrap();
        let a = write_file(&src_dir, "a.mp4", 100);
        let b = write_file(&src_dir, "b.mp4", 100);

        // Budget fits both (100 + 100).
        let mut cache = StagingCache::new(stage_dir.clone(), 300);
        let sa = cache.stage(&a, Some(&b)).unwrap();
        assert!(sa.path().exists());
        assert!(cache.pending.is_some(), "next should be prefetched within budget");
        drop(sa);
        let sb = cache.stage(&b, None).unwrap();
        assert!(sb.path().exists());
        assert_eq!(sb.bytes(), 100);
        drop(sb);
        drop(cache);
        // Everything evicted.
        let remaining: Vec<_> = std::fs::read_dir(&stage_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(remaining.is_empty(), "staging area should be empty after use");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// When the current plus next copy would exceed the budget, copy-ahead is
    /// skipped so total staged bytes stay within the budget; the next file is
    /// staged on demand instead.
    #[test]
    fn copy_ahead_skipped_when_pair_exceeds_budget() {
        let tmp = std::env::temp_dir().join(format!("voloph-stage-test-{}", next_seq()));
        let src_dir = tmp.join("src");
        let stage_dir = tmp.join("stage");
        std::fs::create_dir_all(&src_dir).unwrap();
        let a = write_file(&src_dir, "a.mp4", 100);
        let b = write_file(&src_dir, "b.mp4", 100);

        // Budget fits one but not both.
        let mut cache = StagingCache::new(stage_dir.clone(), 150);
        let sa = cache.stage(&a, Some(&b)).unwrap();
        assert!(cache.pending.is_none(), "pair over budget must not prefetch");
        drop(sa);
        let sb = cache.stage(&b, None).unwrap();
        assert!(sb.path().exists(), "next is staged on demand instead");
        drop(sb);
        drop(cache);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Launch cleanup removes leftover staged files from an interrupted run.
    #[test]
    fn clean_removes_orphans() {
        let tmp = std::env::temp_dir().join(format!("voloph-stage-test-{}", next_seq()));
        let stage_dir = tmp.join("stage");
        std::fs::create_dir_all(&stage_dir).unwrap();
        write_file(&stage_dir, "orphan.mp4", 10);
        assert!(stage_dir.exists());
        clean(&stage_dir);
        assert!(!stage_dir.exists(), "clean should wipe the staging area");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
