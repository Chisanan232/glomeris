//! Bounded, best-effort per-resource size samples (HORO-2362).
//!
//! Volume pressure ([`super::poller`], [`super::stability`]) says the disk is
//! filling; it cannot say which candidate grew or which sat idle. A `free` run
//! writes one sample per candidate it saw, and the next run's receipt reads the
//! previous one back to say "grew X since last run".
//!
//! # No authority
//!
//! These samples are explanation. `policy::classify`, the planner, the executor
//! and Autopilot never read this module, and `tests/run_receipt_has_no_authority.rs`
//! fails if one names it. Age in particular must stay explanation-only: a
//! resource being old or idle is never a reason to delete it.
//!
//! # Failure is "no history"
//!
//! Every failure here degrades to an absent figure, never to a failed command.
//! A missing file is an empty history; an unreadable one is [`HistoryState::Unavailable`]
//! so the receipt can say it has nothing rather than imply nothing grew. A write
//! failure is returned to a caller that is expected to drop it.
//!
//! # Why a file of its own
//!
//! [`crate::workspace::history::store`] holds opaque aliases and counts and is
//! asserted by `tests/workflow_history_holds_no_identity.rs` to hold no byte
//! count. Sizes would break that statement, so they live in a sibling file with
//! the same shape: a versioned JSON envelope, written atomically, read as
//! unusable rather than half-parsed when the version is not this build's.
//!
//! The file holds an opaque key per resource (see
//! [`crate::workspace::history::LocalAlias`]) and no path.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic_write::write_atomically;

/// The only format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// Hard ceiling on tracked resources. Beyond it the least recently sampled go.
pub const MAX_RESOURCES: usize = 256;

/// The largest file this build will read. The bounds above give about 2,000
/// samples; this is far above that and far below anything that would cost memory.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Hard ceiling on samples kept per resource, newest retained.
pub const MAX_SAMPLES_PER_RESOURCE: usize = 8;

/// How long a sample still says something. Ninety days, matching
/// [`crate::workspace::history::RETENTION_WINDOW_SECS`]: long enough to see a
/// campaign target that has been idle for two months.
pub const RETENTION_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// One look at one resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub at_unix_secs: u64,
    /// The size estimate the run had, which may be a lower bound.
    pub bytes: u64,
    /// The resource's own newest modification time, when it was observed.
    pub last_modified_unix_secs: Option<u64>,
}

/// Samples per opaque resource key, each list oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceHistory {
    resources: BTreeMap<u64, Vec<Sample>>,
}

impl ResourceHistory {
    /// A history built from `samples` without touching disk, bounded exactly
    /// as a stored one is.
    pub fn from_samples(now_unix_secs: u64, samples: &[(u64, Sample)]) -> Self {
        Self::default().with_samples(now_unix_secs, samples)
    }

    /// The newest sample for `key` taken strictly before `at_unix_secs` and
    /// inside the retention window. A file written long ago and read now holds
    /// samples nothing has pruned yet; they say nothing about "since last run".
    pub fn latest_before(&self, key: u64, at_unix_secs: u64) -> Option<&Sample> {
        let cutoff = at_unix_secs.saturating_sub(RETENTION_WINDOW_SECS);
        self.resources
            .get(&key)?
            .iter()
            .rev()
            .find(|sample| sample.at_unix_secs < at_unix_secs && sample.at_unix_secs >= cutoff)
    }

    pub fn len(&self) -> usize {
        self.resources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Returns the history with `new` added and both bounds applied.
    fn with_samples(mut self, now_unix_secs: u64, new: &[(u64, Sample)]) -> Self {
        for (key, sample) in new {
            let samples = self.resources.entry(*key).or_default();
            let newest_at = samples.last().map(|s| s.at_unix_secs);
            match newest_at {
                // A clock that went backwards would put the list out of order.
                Some(newest) if sample.at_unix_secs < newest => {}
                Some(newest) if sample.at_unix_secs == newest => {
                    if let Some(last) = samples.last_mut() {
                        *last = *sample;
                    }
                }
                _ => samples.push(*sample),
            }
        }

        // Age before count, as the workflow store does.
        let cutoff = now_unix_secs.saturating_sub(RETENTION_WINDOW_SECS);
        for samples in self.resources.values_mut() {
            samples.retain(|s| s.at_unix_secs >= cutoff);
            if samples.len() > MAX_SAMPLES_PER_RESOURCE {
                let excess = samples.len() - MAX_SAMPLES_PER_RESOURCE;
                samples.drain(0..excess);
            }
        }
        self.resources.retain(|_, samples| !samples.is_empty());

        if self.resources.len() > MAX_RESOURCES {
            let mut by_recency: Vec<(u64, u64)> = self
                .resources
                .iter()
                .filter_map(|(key, samples)| samples.last().map(|s| (s.at_unix_secs, *key)))
                .collect();
            // Oldest newest-sample first, key as the tie-break so the choice
            // does not depend on iteration order.
            by_recency.sort_unstable();
            let excess = self.resources.len() - MAX_RESOURCES;
            for (_, key) in by_recency.into_iter().take(excess) {
                self.resources.remove(&key);
            }
        }
        self
    }
}

/// What reading the file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryState {
    /// Read, or never written (an empty history).
    Available(ResourceHistory),
    /// Present and not usable: unreadable, malformed, or another version.
    /// Distinct from empty so a receipt does not say "nothing grew" when it
    /// could not look.
    Unavailable,
}

/// The per-user file beside the settings file and the other Glomeris state.
pub fn default_resource_history_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("resource-history.json"))
}

/// Reads the history at `path`. Never creates or repairs anything.
pub fn read(path: &Path) -> HistoryState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => return HistoryState::Unavailable,
        _ => {}
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return HistoryState::Available(ResourceHistory::default())
        }
        Err(_) => return HistoryState::Unavailable,
    };
    let stored = match serde_json::from_str::<StoredHistory>(&contents) {
        Ok(stored) if stored.version == FORMAT_VERSION => stored,
        // Malformed, or a version whose fields this build might misread.
        _ => return HistoryState::Unavailable,
    };
    let mut resources = BTreeMap::new();
    for entry in stored.resources {
        let mut samples = entry.samples;
        // A file is not a promise of order.
        samples.sort_by_key(|s| s.at_unix_secs);
        resources.insert(entry.key, samples);
    }
    HistoryState::Available(ResourceHistory { resources })
}

/// Adds `new` to the history at `path`.
///
/// An unreadable existing file is replaced rather than preserved, for the
/// reason [`crate::workspace::history::store::record`] gives: a history that
/// one bad write can disable for good is worse than losing explanatory samples.
///
/// # Errors
///
/// A write failure. Callers report an absent history, not a failed command.
pub fn record(path: &Path, now_unix_secs: u64, new: &[(u64, Sample)]) -> io::Result<()> {
    let held = match read(path) {
        HistoryState::Available(history) => history,
        HistoryState::Unavailable => ResourceHistory::default(),
    };
    let updated = held.with_samples(now_unix_secs, new);
    let stored = StoredHistory {
        version: FORMAT_VERSION,
        resources: updated
            .resources
            .iter()
            .map(|(key, samples)| StoredResource {
                key: *key,
                samples: samples.clone(),
            })
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&stored)
        .map_err(|e| io::Error::other(format!("serializing the resource history: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(path, &json)
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredHistory {
    version: u32,
    resources: Vec<StoredResource>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredResource {
    key: u64,
    samples: Vec<Sample>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;

    fn sample(at: u64, bytes: u64) -> Sample {
        Sample {
            at_unix_secs: at,
            bytes,
            last_modified_unix_secs: Some(at.saturating_sub(DAY)),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-resource-history-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("resource-history.json")
    }

    #[test]
    fn a_missing_file_is_an_empty_available_history() {
        let path = temp_path("missing");
        assert_eq!(
            read(&path),
            HistoryState::Available(ResourceHistory::default())
        );
    }

    #[test]
    fn samples_round_trip_and_the_previous_one_is_found() {
        let path = temp_path("round-trip");
        record(&path, 1_000_000, &[(7, sample(1_000_000, 100))]).unwrap();
        record(&path, 2_000_000, &[(7, sample(2_000_000, 160))]).unwrap();
        let HistoryState::Available(history) = read(&path) else {
            panic!("expected an available history");
        };
        let previous = history.latest_before(7, 3_000_000).unwrap();
        assert_eq!(previous.bytes, 160);
        assert_eq!(history.latest_before(7, 2_000_000).unwrap().bytes, 100);
        assert!(history.latest_before(7, 1_000_000).is_none());
        assert!(history.latest_before(8, 3_000_000).is_none());
    }

    #[test]
    fn a_malformed_or_future_file_is_unavailable_and_is_replaced_on_record() {
        let path = temp_path("bad");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read(&path), HistoryState::Unavailable);
        std::fs::write(&path, r#"{"version":99,"resources":[]}"#).unwrap();
        assert_eq!(read(&path), HistoryState::Unavailable);
        record(&path, 5_000_000, &[(1, sample(5_000_000, 10))]).unwrap();
        assert!(matches!(read(&path), HistoryState::Available(h) if h.len() == 1));
    }

    #[test]
    fn a_directory_where_the_file_belongs_is_unavailable_not_a_panic() {
        let path = temp_path("dir");
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(read(&path), HistoryState::Unavailable);
    }

    #[test]
    fn samples_per_resource_and_resources_are_bounded() {
        let mut history = ResourceHistory::default();
        let now = 100 * DAY;
        for i in 0..(MAX_SAMPLES_PER_RESOURCE as u64 + 5) {
            history = history.with_samples(now, &[(1, sample(now - 20 + i, i))]);
        }
        assert_eq!(history.resources[&1].len(), MAX_SAMPLES_PER_RESOURCE);
        assert_eq!(
            history.resources[&1].last().unwrap().bytes,
            MAX_SAMPLES_PER_RESOURCE as u64 + 4
        );

        let many: Vec<(u64, Sample)> = (0..(MAX_RESOURCES as u64 + 10))
            .map(|key| (key + 1000, sample(now - 1000 + key, 1)))
            .collect();
        let bounded = ResourceHistory::default().with_samples(now, &many);
        assert_eq!(bounded.len(), MAX_RESOURCES);
        // The ten least recently sampled were the ones dropped.
        assert!(!bounded.resources.contains_key(&1000));
        assert!(bounded
            .resources
            .contains_key(&(1000 + MAX_RESOURCES as u64 + 9)));
    }

    #[test]
    fn samples_outside_the_retention_window_are_dropped() {
        let now = 200 * DAY;
        let history = ResourceHistory::default().with_samples(
            now,
            &[
                (1, sample(now - RETENTION_WINDOW_SECS - 1, 5)),
                (2, sample(now - RETENTION_WINDOW_SECS, 5)),
            ],
        );
        assert!(!history.resources.contains_key(&1));
        assert!(history.resources.contains_key(&2));
    }

    #[test]
    fn an_oversized_file_is_unavailable_and_old_samples_are_not_a_previous_run() {
        let path = temp_path("oversized");
        std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(read(&path), HistoryState::Unavailable);

        let now = 300 * DAY;
        let old = ResourceHistory::default().with_samples(
            now - RETENTION_WINDOW_SECS - 10,
            &[(1, sample(now - RETENTION_WINDOW_SECS - 5, 5))],
        );
        assert!(old.latest_before(1, now).is_none());
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_reorder_the_list() {
        let history = ResourceHistory::default()
            .with_samples(1_000, &[(1, sample(1_000, 5))])
            .with_samples(1_000, &[(1, sample(900, 9))]);
        assert_eq!(history.resources[&1], vec![sample(1_000, 5)]);
    }
}
