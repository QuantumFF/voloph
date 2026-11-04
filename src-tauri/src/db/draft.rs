//! A recording's machine draft as the DB and the Analysis files record it (ADR
//! 0013/0019): the stamp naming the segmenter that drew it — which decides when the
//! draft is stale and what its Analysis file is called — and the two row changes
//! every draft goes through: written whole, or cleared for the worker to redraw.

use rusqlite::Connection;

use crate::segment::Segmenter;

/// Which segmenter drew a draft, and which version of it (ADR 0019). Stamped on the
/// recording row and carried in its Analysis. The id stays a string, not a
/// [`Segmenter`]: an id written by a newer build must survive a read and a republish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStamp {
    pub id: String,
    pub version: u32,
}

impl DraftStamp {
    /// The stamp `segmenter` puts on a draft it draws now.
    pub fn of(segmenter: Segmenter) -> DraftStamp {
        DraftStamp {
            id: segmenter.id().to_string(),
            version: segmenter.version(),
        }
    }

    /// The stamp stored on a recording row, or `None` when the row carries no
    /// version: not drawn yet, a pre-versioning draft, or one registered from a
    /// bundle or a carried copy (they never stamp). A NULL id is the legacy v6.
    pub(crate) fn from_row(id: Option<String>, version: Option<i64>) -> Option<DraftStamp> {
        Some(DraftStamp {
            id: id.unwrap_or_else(|| Segmenter::V6.id().to_string()),
            // Clamped, not wrapped, so an out-of-range stored number orders the way
            // the stored integer does.
            version: version?.clamp(0, i64::from(u32::MAX)) as u32,
        })
    }

    /// Whether this draft is current in a library drawing with `segmenter`: made by
    /// it, at its version or newer. Anything else the stale path re-derives and
    /// adoption skips — another segmenter's draft is neither older nor newer.
    pub fn is_current_for(&self, segmenter: Segmenter) -> bool {
        self.id == segmenter.id() && self.version >= segmenter.version()
    }

    /// The draft's Analysis file name in `.voloph/analysis/` for the recording with
    /// content key `{quick_hash}_{file_size}` (ADR 0013). v6 keeps the names older
    /// builds look for — bare at version 1, `_s{N}` after, so each version publishes
    /// **alongside** the last rather than over it. A learned segmenter publishes as
    /// `{key}.{id}{N}.vanalysis`, which older builds' `{key}_s*` match never picks
    /// up, so they cannot mistake a student draft for a v6 one (ADR 0019).
    pub fn file_name(&self, quick_hash: &str, file_size: i64) -> String {
        let key = content_key(quick_hash, file_size);
        if self.id != Segmenter::V6.id() {
            format!("{key}.{}{}.vanalysis", self.id, self.version)
        } else if self.version <= 1 {
            format!("{key}.vanalysis")
        } else {
            format!("{key}_s{}.vanalysis", self.version)
        }
    }
}

/// Whether `name` is one of the Analysis files [`DraftStamp::file_name`] gives the
/// recording keyed `{quick_hash}_{file_size}` — any segmenter, any version; which
/// one made it is read from the file. The `_`/`.` after the key stops `abc_12` from
/// claiming `abc_123`'s files.
pub(crate) fn is_analysis_file_of(name: &str, quick_hash: &str, file_size: i64) -> bool {
    let key = content_key(quick_hash, file_size);
    name == format!("{key}.vanalysis")
        || (name.starts_with(&format!("{key}_s")) || name.starts_with(&format!("{key}.")))
            && name.ends_with(".vanalysis")
}

/// The recording's content key: quick hash + file size, its identity across renames
/// and copies (ADR 0013).
fn content_key(quick_hash: &str, file_size: i64) -> String {
    format!("{quick_hash}_{file_size}")
}

/// Put a machine draft on recording `id`: replace its rallies, store the duration
/// and waveform, mark it segmented, and stamp who drew it (ADR 0013/0019) — so a
/// later bump or a switch of the library's segmenter can tell it is stale, and a
/// republish names it by the segmenter that made it. Run it inside a transaction so
/// a redraw never leaves a half-written timeline.
pub(crate) fn write_draft(
    conn: &Connection,
    id: i64,
    stamp: &DraftStamp,
    duration_ms: Option<i64>,
    waveform: &[f32],
    rallies: impl IntoIterator<Item = (i64, i64, f64)>,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM rallies WHERE recording_id = ?1", [id])?;
    {
        let mut stmt = conn.prepare(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (start_ms, end_ms, confidence) in rallies {
            stmt.execute(rusqlite::params![id, start_ms, end_ms, confidence])?;
        }
    }
    conn.execute(
        "UPDATE recordings
         SET segment_state = 'ready', duration_ms = ?1, waveform = ?2, segmenter_version = ?3,
             segmenter_id = ?4
         WHERE id = ?5",
        rusqlite::params![
            duration_ms,
            super::waveform_to_json(waveform),
            stamp.version,
            stamp.id,
            id
        ],
    )?;
    Ok(())
}

/// Which recordings' drafts [`clear_drafts`] drops.
pub(crate) enum Drafts<'a> {
    /// The one recording with this row id.
    Of(i64),
    /// Every recording in the library of this kind — Re-analyze all, two statements
    /// however large the library.
    InLibrary(&'a str),
}

/// Drop drafts so the worker draws them again: no rallies, `unknown`, and nothing
/// the draft brought (duration, waveform, stamp). `probe_state` stays — the bytes
/// did not change. Every path that discards a draft goes through here, so none can
/// leave a stale stamp on an undrawn row.
pub(crate) fn clear_drafts(conn: &Connection, which: Drafts) -> rusqlite::Result<()> {
    let (filter, param): (&str, &dyn rusqlite::ToSql) = match &which {
        Drafts::Of(id) => ("id = ?1", id),
        Drafts::InLibrary(kind) => ("library = ?1", kind),
    };
    conn.execute(
        &format!(
            "DELETE FROM rallies WHERE recording_id IN (SELECT id FROM recordings WHERE {filter})"
        ),
        [param],
    )?;
    conn.execute(
        &format!(
            "UPDATE recordings
             SET segment_state = 'unknown', duration_ms = NULL, waveform = NULL,
                 segmenter_version = NULL, segmenter_id = NULL
             WHERE {filter}"
        ),
        [param],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(id: &str, version: u32) -> DraftStamp {
        DraftStamp {
            id: id.to_string(),
            version,
        }
    }

    #[test]
    fn a_row_without_a_version_has_no_stamp_and_a_null_id_is_v6() {
        assert_eq!(DraftStamp::from_row(None, None), None);
        assert_eq!(DraftStamp::from_row(Some("student".into()), None), None);
        assert_eq!(DraftStamp::from_row(None, Some(7)), Some(stamp("v6", 7)));
        assert_eq!(
            DraftStamp::from_row(Some("coach".into()), Some(3)),
            Some(stamp("coach", 3))
        );
        assert_eq!(DraftStamp::from_row(None, Some(-1)), Some(stamp("v6", 0)));
    }

    #[test]
    fn a_stamp_is_current_only_for_its_own_segmenter_at_its_version_or_newer() {
        use crate::segment::SEGMENTER_VERSION as V6;
        for s in Segmenter::ALL {
            assert!(DraftStamp::of(s).is_current_for(s), "{s:?}");
            assert!(stamp(s.id(), s.version() + 1).is_current_for(s), "{s:?}");
            if s.version() > 0 {
                assert!(!stamp(s.id(), s.version() - 1).is_current_for(s), "{s:?}");
            }
            for other in Segmenter::ALL.into_iter().filter(|&o| o != s) {
                assert!(
                    !DraftStamp::of(s).is_current_for(other),
                    "{s:?} in {other:?}"
                );
            }
            assert!(!stamp("coach", 99).is_current_for(s));
        }
        assert!(!stamp("v6", V6 - 1).is_current_for(Segmenter::V6));
    }

    #[test]
    fn every_published_name_reads_back_as_its_own_keys_and_no_other() {
        let stamps = [
            stamp("v6", 0),
            stamp("v6", 1),
            stamp("v6", 7),
            stamp("student", 1),
            stamp("teacher", 1),
            stamp("coach", 3),
        ];
        for s in &stamps {
            let name = s.file_name("abc", 12);
            assert!(is_analysis_file_of(&name, "abc", 12), "{name}");
            assert!(!is_analysis_file_of(&name, "abc", 123), "{name}");
            assert!(!is_analysis_file_of(&name, "abc", 1), "{name}");
            assert!(!is_analysis_file_of(&name, "abd", 12), "{name}");
        }
        for name in [
            ".abc_12_s9.vanalysis.tmp",
            "abc_12_s9.vanalysis.bak",
            "abc_12.vbundle",
            "abc_12",
        ] {
            assert!(!is_analysis_file_of(name, "abc", 12), "{name}");
        }
    }
}
