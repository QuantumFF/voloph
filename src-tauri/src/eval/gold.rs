//! **Gold files** — the eval harness's ground truth, kept *outside* the app DB.
//!
//! A gold file is one recording's fully verified rally timeline, written by an
//! explicit `eval-harness --export-gold` once a human has checked the whole
//! recording. The harness scores only against these files, never against the live
//! DB timeline, for two reasons:
//!
//! - **The DB can't tell gold from a draft.** The hand-correction sentinel
//!   ([`crate::db::CORRECTED_CONFIDENCE`], `1.0`) collides with machine output: the
//!   segmenter clamps a fully-active span's confidence to `1.0` too, so a fresh draft
//!   looked "hand-corrected" and the harness would score the segmenter against itself.
//!   Exporting is a deliberate act, so a gold file means "a human verified all of it".
//! - **The DB is disposable.** Recreating it (it happened — the original 21-06 gold
//!   was lost with one) must not destroy the eval corpus. Gold files are small JSON
//!   that can live in git and be backed up with the recordings.
//!
//! A file is matched to a recording by **content identity** — file size plus the
//! app's [`quick_hash`](crate::db::scan) — never by path, so gold survives moving the
//! recordings or re-pointing the library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Interval;

/// Current on-disk format. Bump only with a reader for the old one.
pub const FORMAT: u32 = 1;

/// One recording's verified timeline as stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldFile {
    pub format: u32,
    /// The recording's file name — for humans; matching uses size + hash.
    pub recording: String,
    pub file_size: i64,
    pub quick_hash: String,
    pub duration_ms: Option<i64>,
    /// Unix seconds when the export ran (when a human declared it verified).
    pub verified_unix: u64,
    /// Rally intervals `[start_ms, end_ms]`, sorted and non-overlapping.
    pub rallies: Vec<[i64; 2]>,
}

impl GoldFile {
    pub fn intervals(&self) -> Vec<Interval> {
        self.rallies
            .iter()
            .map(|&[start_ms, end_ms]| Interval { start_ms, end_ms })
            .collect()
    }
}

/// Check a gold timeline is well-formed: every rally has positive length, rallies
/// are sorted and do not overlap (touching is fine), and none runs past the
/// recording's duration when that is known.
pub fn validate(rallies: &[[i64; 2]], duration_ms: Option<i64>) -> Result<(), String> {
    let mut prev_end = i64::MIN;
    for (i, &[start, end]) in rallies.iter().enumerate() {
        if start < 0 || end <= start {
            return Err(format!("rally {i} [{start}, {end}] is empty or negative"));
        }
        if start < prev_end {
            return Err(format!("rally {i} [{start}, {end}] overlaps or precedes the one before it"));
        }
        if let Some(d) = duration_ms.filter(|&d| d > 0) {
            if end > d {
                return Err(format!("rally {i} [{start}, {end}] ends past the recording ({d} ms)"));
            }
        }
        prev_end = end;
    }
    Ok(())
}

/// The file name a recording's gold is stored under: its file name plus a short
/// hash prefix, so two cameras' `IMG_0001.MOV` never collide in one flat dir.
pub fn file_name(recording: &str, quick_hash: &str) -> String {
    timeline_file_name(recording, quick_hash, "gold")
}

/// [`file_name`] for any timeline `kind` (`gold`, `draft`, …). Only `gold` files are
/// ever read back by [`load_dir`], so a dumped draft can never pass as gold.
pub fn timeline_file_name(recording: &str, quick_hash: &str, kind: &str) -> String {
    let base = Path::new(recording)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| recording.to_string());
    let short: String = quick_hash.chars().take(12).collect();
    format!("{base}.{short}.{kind}.json")
}

/// Every gold file in `dir`, keyed by `(file_size, quick_hash)`. A missing dir is an
/// empty corpus; an unreadable or malformed file is a hard error (never silently
/// dropped from the corpus).
pub fn load_dir(dir: &Path) -> Result<HashMap<(i64, String), GoldFile>, String> {
    let mut out = HashMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("cannot read gold dir {}: {e}", dir.display())),
    };
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.to_string_lossy().ends_with(".gold.json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let gold: GoldFile = serde_json::from_str(&text)
            .map_err(|e| format!("malformed gold file {}: {e}", path.display()))?;
        if gold.format != FORMAT {
            return Err(format!("{}: unsupported gold format {}", path.display(), gold.format));
        }
        validate(&gold.rallies, gold.duration_ms).map_err(|e| format!("{}: {e}", path.display()))?;
        let key = (gold.file_size, gold.quick_hash.clone());
        if let Some(prev) = out.insert(key, gold) {
            return Err(format!(
                "two gold files in {} describe the same recording ({})",
                dir.display(),
                prev.recording
            ));
        }
    }
    Ok(out)
}

/// Write `gold` into `dir` (created if needed), replacing any earlier export of the
/// same recording. Written to a temp file and renamed, so a crash never leaves a
/// half-written gold file. Returns the path written.
pub fn save(dir: &Path, gold: &GoldFile) -> Result<PathBuf, String> {
    save_timeline(dir, gold, "gold")
}

/// [`save`] for any timeline `kind` — the same schema, a different suffix.
pub fn save_timeline(dir: &Path, gold: &GoldFile, kind: &str) -> Result<PathBuf, String> {
    validate(&gold.rallies, gold.duration_ms)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(timeline_file_name(&gold.recording, &gold.quick_hash, kind));
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(gold).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text + "\n").map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> GoldFile {
        GoldFile {
            format: FORMAT,
            recording: "21-06-2026/IMG_5930.MOV".into(),
            file_size: 1_362_822_145,
            quick_hash: "abcdef0123456789abcdef".into(),
            duration_ms: Some(60_000),
            verified_unix: 1_700_000_000,
            rallies: vec![[1_000, 5_000], [5_000, 9_000], [20_000, 30_000]],
        }
    }

    #[test]
    fn validate_accepts_sorted_touching_rallies() {
        assert_eq!(validate(&sample().rallies, Some(60_000)), Ok(()));
        assert_eq!(validate(&[], None), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_timelines() {
        assert!(validate(&[[5_000, 5_000]], None).is_err(), "zero length");
        assert!(validate(&[[-1, 5_000]], None).is_err(), "negative start");
        assert!(validate(&[[1_000, 5_000], [4_000, 6_000]], None).is_err(), "overlap");
        assert!(validate(&[[4_000, 6_000], [1_000, 2_000]], None).is_err(), "unsorted");
        assert!(validate(&[[1_000, 61_000]], Some(60_000)).is_err(), "past the end");
    }

    #[test]
    fn file_name_uses_basename_and_hash_prefix() {
        assert_eq!(
            file_name("21-06-2026/IMG_5930.MOV", "abcdef0123456789abcdef"),
            "IMG_5930.MOV.abcdef012345.gold.json"
        );
    }

    #[test]
    fn save_then_load_round_trips_keyed_by_identity() {
        let dir = std::env::temp_dir().join(format!("voloph-gold-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let gold = sample();
        save(&dir, &gold).unwrap();
        // Re-exporting the same recording replaces, not duplicates.
        save(&dir, &gold).unwrap();
        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[&(gold.file_size, gold.quick_hash.clone())], gold);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drafts_in_the_gold_dir_are_never_loaded_as_gold() {
        let dir = std::env::temp_dir().join(format!("voloph-gold-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_timeline(&dir, &sample(), "draft").unwrap();
        assert!(load_dir(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_dir_is_an_empty_corpus() {
        let dir = std::env::temp_dir().join("voloph-gold-test-does-not-exist");
        assert!(load_dir(&dir).unwrap().is_empty());
    }

    #[test]
    fn malformed_file_is_an_error_not_a_skip() {
        let dir = std::env::temp_dir().join(format!("voloph-gold-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.gold.json"), "{ not json").unwrap();
        assert!(load_dir(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
