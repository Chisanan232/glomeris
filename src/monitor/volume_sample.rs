//! Bounded, privacy-safe capacity-sample history (HORO-1827 / ADR-0001 §4.4).
//!
//! A [`VolumeSample`] is deliberately the smallest thing that can answer
//! "how has this volume's free space moved over the last several hours":
//! a timestamp, an opaque mount identity, and the two numbers `statvfs`
//! already gives [`crate::monitor::fs_stat::FsStat::stat`] every poll. No
//! path, no process name, nothing that identifies a file or a user — see
//! ADR-0001 §6's privacy row and this module's tests.
//!
//! Storage is a fixed-capacity ring, not an append-only log: unlike
//! `history.tsv` (pressure transitions, rare) or `actions.jsonl` (real
//! executions, rare), a capacity sample is taken every few minutes for as
//! long as the opt-in setting is on, so an unbounded file here would grow
//! without limit for the life of the daemon. [`MAX_SAMPLES`] bounds it
//! structurally: the ring can never hold more than that many samples, in
//! memory or on disk, regardless of how long sampling has been running.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io;
use std::path::Path;

/// One capacity observation for one mounted filesystem.
///
/// `volume_dev` is the mount's `st_dev` — an opaque per-boot integer, never
/// a path — so that a sample taken against a different filesystem (an
/// external disk swapped in, or a different mount entirely) can be told
/// apart from a continuous run of samples against the same one (ADR-0001
/// §4.4: "different filesystem" => the earlier window does not apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSample {
    pub unix_secs: u64,
    pub volume_dev: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl VolumeSample {
    pub fn new(unix_secs: u64, volume_dev: u64, total_bytes: u64, free_bytes: u64) -> Self {
        Self {
            unix_secs,
            volume_dev,
            total_bytes,
            free_bytes,
        }
    }
}

/// Maximum number of samples ever retained, in memory or on disk.
///
/// At the ~15 minute cadence this ticket calls for, 288 samples is roughly
/// three days of history — comfortably more than the 12h window
/// [`crate::monitor::stability`] requires for an honest verdict, while
/// still bounding storage to a small, fixed size regardless of how long the
/// opt-in setting has been on.
pub const MAX_SAMPLES: usize = 288;

/// How many 60-second poll ticks (see [`crate::monitor::poller::default_poll_interval`])
/// make up one capacity sample — 15, for roughly a 15-minute cadence, the
/// upper end of this ticket's "~10-15min" acceptance criterion.
///
/// A fixed constant rather than a user setting on purpose: the opt-in flag
/// in [`crate::settings::RecoverySettings`] already lets a user turn
/// sampling off entirely, and a second knob for *how often* would be a
/// second way the 12h stability window's assumptions (`expected_interval_secs`
/// in [`crate::monitor::stability::analyze`]) could silently drift out of
/// sync with what is actually being written.
pub const SAMPLE_EVERY_N_TICKS: u64 = 15;

/// A bounded, oldest-evicted-first ring of [`VolumeSample`]s.
#[derive(Debug, Clone, Default)]
pub struct VolumeSampleRing {
    samples: VecDeque<VolumeSample>,
}

impl VolumeSampleRing {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
        }
    }

    /// Builds a ring from already-persisted samples, keeping only the most
    /// recent [`MAX_SAMPLES`] if the input is longer — defends against a
    /// hand-edited or foreign-written file claiming to hold more than this
    /// build will ever write itself.
    pub fn from_samples(samples: Vec<VolumeSample>) -> Self {
        let mut ring = Self::new();
        for sample in samples {
            ring.push(sample);
        }
        ring
    }

    /// Appends `sample`, evicting the oldest entry first if the ring is
    /// already at [`MAX_SAMPLES`]. This is the single structural guarantee
    /// that storage stays bounded: nothing about the ring's own
    /// representation is capable of exceeding [`MAX_SAMPLES`].
    pub fn push(&mut self, sample: VolumeSample) {
        if self.samples.len() >= MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The samples, oldest first — the same order [`push`](Self::push)
    /// appends in.
    pub fn as_slice_vec(&self) -> Vec<VolumeSample> {
        self.samples.iter().copied().collect()
    }
}

/// Reads back the samples previously written by [`write_samples`].
///
/// Degrades to an empty `Vec` on any error (missing file, unreadable,
/// malformed JSON) — matching [`crate::monitor::persistence::read_heartbeat`]
/// and [`crate::monitor::persistence::read_history_tail`]'s contract: a
/// reader of this best-effort store must never treat "nothing recorded yet"
/// or "the file is corrupt" as fatal.
pub fn read_samples(path: &Path) -> Vec<VolumeSample> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Writes the ring's current contents to `path` as a JSON array, atomically
/// (see [`crate::atomic_write::write_atomically`]) so a concurrent reader
/// never observes a torn or partially-written file.
///
/// The whole ring is rewritten on every call rather than appended to —
/// unlike `history.tsv`/`actions.jsonl`, this store is already bounded by
/// [`MAX_SAMPLES`], so "rewrite everything" is at most a few hundred small
/// JSON objects, and rewriting is what keeps the on-disk file bounded too:
/// an append-only file would grow forever even though the logical ring does
/// not.
///
/// Best-effort by contract, matching every other store in
/// [`crate::monitor::persistence`]: a caller must treat any error here as
/// non-fatal to the poll loop.
pub fn write_samples(path: &Path, ring: &VolumeSampleRing) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec(&ring.as_slice_vec()).map_err(io::Error::from)?;
    crate::atomic_write::write_atomically(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(secs: u64, free: u64) -> VolumeSample {
        VolumeSample::new(secs, 42, 1_000_000_000, free)
    }

    #[test]
    fn ring_never_grows_past_max_samples() {
        let mut ring = VolumeSampleRing::new();
        for i in 0..(MAX_SAMPLES * 3) {
            ring.push(sample(i as u64, 500));
        }
        assert_eq!(ring.len(), MAX_SAMPLES);
    }

    #[test]
    fn ring_evicts_oldest_first() {
        let mut ring = VolumeSampleRing::new();
        for i in 0..(MAX_SAMPLES + 5) {
            ring.push(sample(i as u64, 500));
        }
        let samples = ring.as_slice_vec();
        // The first 5 (timestamps 0..5) must have been evicted.
        assert_eq!(samples.first().unwrap().unix_secs, 5);
        assert_eq!(samples.last().unwrap().unix_secs, (MAX_SAMPLES + 4) as u64);
    }

    #[test]
    fn from_samples_bounds_an_oversized_input() {
        let oversized: Vec<VolumeSample> = (0..(MAX_SAMPLES * 2))
            .map(|i| sample(i as u64, 500))
            .collect();
        let ring = VolumeSampleRing::from_samples(oversized);
        assert_eq!(ring.len(), MAX_SAMPLES);
    }

    #[test]
    fn a_persisted_sample_carries_no_path_or_process_identity() {
        // Structural privacy guard (ADR-0001 §6): serialize a sample and
        // confirm its JSON contains only the four numeric fields it
        // declares — nothing that could ever hold a path or process name.
        let s = sample(1_700_000_000, 123);
        let json = serde_json::to_value(s).unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["free_bytes", "total_bytes", "unix_secs", "volume_dev"]
        );
    }

    fn unique_samples_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-volume-samples-test-{tag}-{}-{}",
            std::process::id(),
            crate::monitor::persistence::unix_now_secs()
        ))
    }

    #[test]
    fn samples_round_trip_through_write_and_read() {
        let path = unique_samples_path("round-trip");
        let mut ring = VolumeSampleRing::new();
        ring.push(sample(1, 100));
        ring.push(sample(2, 90));

        write_samples(&path, &ring).expect("write should succeed");
        let read_back = read_samples(&path);

        assert_eq!(read_back, ring.as_slice_vec());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_samples_returns_empty_for_missing_file() {
        let path = unique_samples_path("missing");
        assert!(read_samples(&path).is_empty());
    }

    #[test]
    fn read_samples_returns_empty_for_malformed_contents() {
        let path = unique_samples_path("malformed");
        std::fs::write(&path, b"not json").unwrap();
        assert!(read_samples(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_samples_persists_at_most_max_samples() {
        let path = unique_samples_path("bounded-on-disk");
        let mut ring = VolumeSampleRing::new();
        for i in 0..(MAX_SAMPLES * 2) {
            ring.push(sample(i as u64, 500));
        }
        write_samples(&path, &ring).expect("write should succeed");
        let read_back = read_samples(&path);
        assert_eq!(read_back.len(), MAX_SAMPLES);
        let _ = std::fs::remove_file(&path);
    }
}
