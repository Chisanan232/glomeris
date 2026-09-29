//! The bounded local file the workflow baseline is made of (HORO-1547).
//!
//! # Three bounds, and what each is for
//!
//! [`MAX_OBSERVATIONS`] bounds the file. It holds unconditionally: whatever
//! happens, the store is at most this many records, so nothing here can grow
//! without limit on a machine nobody is watching.
//!
//! [`RETENTION_WINDOW_SECS`] bounds relevance. It is applied twice, on purpose.
//! On write, records outside the window are dropped, so the file stays small.
//! On read — by [`super::classify`], which is the half that knows what "now"
//! is — records outside the window are ignored, so a file last written two
//! years ago cannot speak for today. A write-side bound alone would leave that
//! stale file being believed; a read-side bound alone would leave it growing.
//!
//! [`MIN_ADMISSION_INTERVAL_SECS`] bounds *what counts as an observation*, and
//! it is the one that stops this whole feature from being able to lie. Without
//! it, five runs in a minute are five observations, and
//! [`crate::workspace::MIN_OBSERVATIONS_FOR_A_PATTERN`] is satisfied by a
//! single `for` loop — the summary would say "observed" while resting on one
//! moment seen five times. HORO-1547's second acceptance criterion is that the
//! system cannot claim "usually" from a single observation, and a bare count of
//! samples does not deliver that. Spacing does.
//!
//! # Why JSON
//!
//! Same reasoning as [`crate::monitor::episode_store`], which this file follows
//! closely: nested records, so the `key = value` settings format cannot express
//! it, and a versioned envelope whose unknown versions are unreadable rather
//! than half-parsed. Written through [`crate::atomic_write::write_atomically`],
//! so a full disk or a kill mid-write leaves the previous file rather than a
//! truncated one.
//!
//! # Why unreadable is not absent
//!
//! [`crate::monitor::episode_store`] collapses every read failure into `None`,
//! and explains why: there, forgetting an open pressure episode costs at worst
//! a redundant notification. Here the distinction is load-bearing, because
//! HORO-1547's fifth acceptance criterion is about exactly it. A store that was
//! never written and a store that cannot be parsed are different facts about
//! this machine, they reach the model as different [`crate::evidence::ProbeReason`]s,
//! and a reader shown one when the other is true has been told the baseline was
//! never attempted when in truth it broke. Hence [`StoreState`] with three
//! variants and no `Option` anywhere near it.
//!
//! # What is not in the file
//!
//! No path, no branch name, no repository name, no username, no file contents,
//! no byte counts, and nothing about current activity. Only timestamps, small
//! counts, and the opaque aliases of [`super::alias`] — see that module for what
//! the aliasing does and does not buy, and
//! `tests/workflow_history_holds_no_identity.rs` for the assertion that keeps it
//! true.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic_write::write_atomically;
use crate::evidence::probe::ProbeReason;

use super::alias::LocalAlias;
use super::observation::{RepositoryObservation, WorkspaceObservation};

/// The only format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// The hard ceiling on records in the file.
///
/// Sixty-four spaced at least an hour apart is several weeks of working days,
/// which is more than any classification here consults, and at roughly a
/// hundred bytes a record the file stays a few kilobytes at its largest.
pub const MAX_OBSERVATIONS: usize = 64;

/// How far back an observation still says something about how someone works.
///
/// Ninety days. Long enough that a month between bursts of work on a project
/// does not erase the baseline, short enough that a way of working abandoned
/// last spring is not being reported as current practice.
pub const RETENTION_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// The minimum gap between two admitted observations.
///
/// One hour. The unit being counted is "a time I looked at this machine", and
/// two looks a minute apart are one look. See the module docs for why a count
/// of samples without this is not evidence of a habit.
pub const MIN_ADMISSION_INTERVAL_SECS: u64 = 60 * 60;

/// What the store had to say.
///
/// Three variants because three things are true of a store, and the campaign's
/// rule that a failed probe is not an empty answer applies to a file exactly as
/// it applies to a subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    /// Nothing is there. No baseline has ever been collected on this machine.
    NeverCollected,
    /// Something is there and this build cannot use it: unreadable, malformed,
    /// or written by a format version this build does not know.
    ///
    /// The reason is carried so the distinction survives all the way to the
    /// model, where `failed` and `not_attempted` are different words.
    Unreadable(ProbeReason),
    /// Read successfully. Observations are ordered oldest first, and the list
    /// may be empty — a store whose every record fell outside the retention
    /// window is a collected baseline with nothing left in it, which is not the
    /// same as never having collected one.
    Collected(Vec<WorkspaceObservation>),
}

/// Whether a new observation was counted, and why not when it was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Counted. The store now holds it.
    Admitted,
    /// Not counted: too soon after the newest record already held.
    ///
    /// Carries how long until one would be, so the surface reporting this can
    /// say something true rather than a bare refusal.
    TooSoon { seconds_until_eligible: u64 },
    /// Not counted: the observation is older than one already held, which means
    /// the clock moved backwards between runs.
    ///
    /// Admitting it would put the file out of order, and — because spacing is
    /// measured against the newest record — would let a clock that keeps
    /// jumping back admit an unbounded number of observations of one moment.
    /// Refusing costs one sample.
    ClockWentBackwards,
}

/// Resolves the per-user store path: the same
/// `~/Library/Application Support/Glomeris/` directory as the settings file,
/// the heartbeat and the pressure episode, and the same missing-`HOME`
/// handling.
pub fn default_history_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("workflow-history.json"))
}

/// Reads the store at `path`.
///
/// Never creates anything and never repairs anything: a caller that only wants
/// to *show* the baseline must be able to do so without writing to disk.
pub fn read(path: &Path) -> StoreState {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return StoreState::NeverCollected,
        // Present but unreadable — a permission problem, a directory where a
        // file should be, an I/O error. Not an absence.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return StoreState::Unreadable(ProbeReason::PermissionDenied)
        }
        Err(_) => return StoreState::Unreadable(ProbeReason::Failed),
    };

    let stored: StoredState = match serde_json::from_str(&contents) {
        Ok(stored) => stored,
        Err(_) => return StoreState::Unreadable(ProbeReason::Failed),
    };
    if stored.version != FORMAT_VERSION {
        // Including a *newer* version, which is the case that matters: a future
        // build's records read through this build's assumptions would be
        // counts of something else. `Failed` rather than a reason of its own —
        // what the reader needs to know is that there is a store and it cannot
        // be used.
        return StoreState::Unreadable(ProbeReason::Failed);
    }

    let mut observations: Vec<WorkspaceObservation> =
        stored.observations.into_iter().map(Into::into).collect();
    // The file is written in order, but a file is not a promise. Sorting here
    // means the spacing check and the retention window are correct even for a
    // hand-edited or concatenated file.
    observations.sort_by_key(|o| o.at_unix_secs);
    StoreState::Collected(observations)
}

/// Offers `observation` to the store at `path`.
///
/// Returns what the store decided and the observations it holds afterwards, so
/// a caller can classify without reading the file again.
///
/// Writes only when the observation is admitted *or* when compaction actually
/// removed something: a refused observation on an already-tidy store touches no
/// file, which is what makes calling this on a timer cheap.
///
/// # Errors
///
/// A write failure. Reading never fails here — an unreadable store is
/// *replaced*, because the alternative is a machine whose baseline can never
/// recover from one bad write, and the records being replaced are explanatory
/// aggregates rather than anything a user could not afford to lose. The return
/// value says an empty store was the starting point, so nothing silently claims
/// the replaced records as evidence.
pub fn record(
    path: &Path,
    observation: WorkspaceObservation,
) -> io::Result<(Admission, Vec<WorkspaceObservation>)> {
    let mut held = match read(path) {
        StoreState::Collected(observations) => observations,
        StoreState::NeverCollected | StoreState::Unreadable(_) => Vec::new(),
    };

    if let Some(newest) = held.last() {
        if observation.at_unix_secs < newest.at_unix_secs {
            return Ok((Admission::ClockWentBackwards, held));
        }
        let gap = observation.at_unix_secs - newest.at_unix_secs;
        if gap < MIN_ADMISSION_INTERVAL_SECS {
            return Ok((
                Admission::TooSoon {
                    seconds_until_eligible: MIN_ADMISSION_INTERVAL_SECS - gap,
                },
                held,
            ));
        }
    }

    let now = observation.at_unix_secs;
    held.push(observation);
    compact(&mut held, now);

    let stored = StoredState {
        version: FORMAT_VERSION,
        observations: held.iter().map(StoredObservation::from).collect(),
    };
    let json = serde_json::to_vec_pretty(&stored)
        .map_err(|e| io::Error::other(format!("serializing the workflow history: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(path, &json)?;

    Ok((Admission::Admitted, held))
}

/// Applies both bounds, oldest first.
///
/// Age before count, so a burst of recent observations cannot push out records
/// that are still inside the window while stale ones survive on recency alone.
fn compact(observations: &mut Vec<WorkspaceObservation>, now_unix_secs: u64) {
    let cutoff = now_unix_secs.saturating_sub(RETENTION_WINDOW_SECS);
    observations.retain(|o| o.at_unix_secs >= cutoff);
    if observations.len() > MAX_OBSERVATIONS {
        let excess = observations.len() - MAX_OBSERVATIONS;
        observations.drain(0..excess);
    }
}

// ---------------------------------------------------------------------------
// The stored shapes
//
// Separate from the domain types on purpose, and for the same reason
// `crate::monitor::persistence` builds its own `Heartbeat` rather than
// deriving `Serialize` on a domain type: adding a field to
// `RepositoryObservation` must not silently change what is written to a user's
// disk, and adding one here must not silently change what the classifier reads.
// The conversions below are the one place the two are joined, and
// `the_stored_shape_carries_every_field_the_classifier_reads` is what notices
// when one of them stops.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredState {
    version: u32,
    observations: Vec<StoredObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredObservation {
    at_unix_secs: u64,
    repositories: Vec<StoredRepository>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredRepository {
    /// An opaque alias, as a number. No path, no name.
    repository: u64,
    worktree_count: u32,
    linked_worktree_count: u32,
    detached_worktree_count: u32,
    /// `null` when there was no single checkout on a branch to record.
    single_checkout_branch: Option<u64>,
}

impl From<&WorkspaceObservation> for StoredObservation {
    fn from(observation: &WorkspaceObservation) -> Self {
        Self {
            at_unix_secs: observation.at_unix_secs,
            repositories: observation
                .repositories
                .iter()
                .map(|repo| StoredRepository {
                    repository: repo.repository.as_u64(),
                    worktree_count: repo.worktree_count,
                    linked_worktree_count: repo.linked_worktree_count,
                    detached_worktree_count: repo.detached_worktree_count,
                    single_checkout_branch: repo.single_checkout_branch.map(LocalAlias::as_u64),
                })
                .collect(),
        }
    }
}

impl From<StoredObservation> for WorkspaceObservation {
    fn from(stored: StoredObservation) -> Self {
        Self {
            at_unix_secs: stored.at_unix_secs,
            repositories: stored
                .repositories
                .into_iter()
                .map(|repo| RepositoryObservation {
                    repository: LocalAlias::from_u64(repo.repository),
                    worktree_count: repo.worktree_count,
                    linked_worktree_count: repo.linked_worktree_count,
                    detached_worktree_count: repo.detached_worktree_count,
                    single_checkout_branch: repo.single_checkout_branch.map(LocalAlias::from_u64),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::history::fixtures::temp_path;

    fn observation(at: u64, worktrees: u32) -> WorkspaceObservation {
        WorkspaceObservation {
            at_unix_secs: at,
            repositories: vec![RepositoryObservation {
                repository: LocalAlias::of_name("repo"),
                worktree_count: worktrees,
                linked_worktree_count: worktrees.saturating_sub(1),
                detached_worktree_count: 0,
                single_checkout_branch: (worktrees == 1).then(|| LocalAlias::of_name("main")),
            }],
        }
    }

    fn collected(path: &Path) -> Vec<WorkspaceObservation> {
        match read(path) {
            StoreState::Collected(observations) => observations,
            other => panic!("expected a readable store, got {other:?}"),
        }
    }

    const HOUR: u64 = MIN_ADMISSION_INTERVAL_SECS;

    // -----------------------------------------------------------------
    // The three read outcomes (AC 5)
    // -----------------------------------------------------------------

    #[test]
    fn an_absent_store_was_never_collected_rather_than_empty() {
        let path = temp_path("absent").join("workflow-history.json");
        assert_eq!(read(&path), StoreState::NeverCollected);
    }

    #[test]
    fn a_malformed_store_is_unreadable_rather_than_never_collected() {
        let path = temp_path("malformed");
        std::fs::write(&path, b"{ not json").expect("write");
        assert_eq!(read(&path), StoreState::Unreadable(ProbeReason::Failed));
    }

    /// The case that would otherwise be silent: a build from the future wrote
    /// records this build would misread as its own.
    #[test]
    fn a_future_format_version_is_unreadable_rather_than_reinterpreted() {
        let path = temp_path("future");
        std::fs::write(
            &path,
            format!(r#"{{"version":{},"observations":[]}}"#, FORMAT_VERSION + 1),
        )
        .expect("write");
        assert_eq!(read(&path), StoreState::Unreadable(ProbeReason::Failed));
    }

    /// A readable store with nothing left in it after retention is a collected
    /// baseline that has run dry. Reporting it as never-collected would claim
    /// this machine has never been looked at.
    #[test]
    fn a_readable_but_empty_store_is_collected_with_nothing_in_it() {
        let path = temp_path("empty");
        std::fs::write(
            &path,
            format!(r#"{{"version":{FORMAT_VERSION},"observations":[]}}"#),
        )
        .expect("write");
        assert_eq!(read(&path), StoreState::Collected(Vec::new()));
    }

    // -----------------------------------------------------------------
    // Admission spacing (AC 2)
    // -----------------------------------------------------------------

    /// The anti-vacuity property of this file. Five runs in one minute are one
    /// look at the machine, and if they counted as five the summary would reach
    /// "observed" from a single moment.
    #[test]
    fn repeated_runs_within_the_interval_do_not_become_several_observations() {
        let path = temp_path("burst");
        for i in 0..5 {
            let (admission, held) = record(&path, observation(1_000_000 + i, 1)).expect("record");
            if i == 0 {
                assert_eq!(admission, Admission::Admitted);
            } else {
                assert!(
                    matches!(admission, Admission::TooSoon { .. }),
                    "run {i} was admitted {admission:?} — five runs a second apart became \
                     five observations, and a habit can be claimed from one moment"
                );
            }
            assert_eq!(held.len(), 1);
        }
        assert_eq!(collected(&path).len(), 1);
    }

    #[test]
    fn an_observation_a_full_interval_later_is_admitted() {
        let path = temp_path("spaced");
        record(&path, observation(1_000_000, 1)).expect("record");
        let (admission, held) = record(&path, observation(1_000_000 + HOUR, 1)).expect("record");
        assert_eq!(admission, Admission::Admitted);
        assert_eq!(held.len(), 2);
    }

    #[test]
    fn a_refusal_says_how_long_until_one_would_be_admitted() {
        let path = temp_path("countdown");
        record(&path, observation(1_000_000, 1)).expect("record");
        let (admission, _) = record(&path, observation(1_000_000 + 600, 1)).expect("record");
        assert_eq!(
            admission,
            Admission::TooSoon {
                seconds_until_eligible: HOUR - 600
            }
        );
    }

    /// A clock that jumps backwards must not be able to admit an unbounded
    /// number of observations of one moment — spacing is measured against the
    /// newest record, so an older record would leave every later one eligible.
    #[test]
    fn an_observation_older_than_the_newest_is_refused() {
        let path = temp_path("backwards");
        record(&path, observation(1_000_000, 1)).expect("record");
        let (admission, held) = record(&path, observation(999_000, 1)).expect("record");
        assert_eq!(admission, Admission::ClockWentBackwards);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].at_unix_secs, 1_000_000);
    }

    /// A refused observation must not rewrite the file. Otherwise a timer
    /// calling this every minute rewrites a user's disk every minute to store
    /// the same bytes.
    #[test]
    fn a_refused_observation_leaves_the_file_untouched() {
        let path = temp_path("untouched");
        record(&path, observation(1_000_000, 1)).expect("record");
        let before = std::fs::read(&path).expect("read");
        let modified_before = std::fs::metadata(&path).and_then(|m| m.modified());

        record(&path, observation(1_000_060, 1)).expect("record");

        assert_eq!(std::fs::read(&path).expect("read"), before);
        assert_eq!(
            std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            modified_before.ok(),
            "the file was rewritten for an observation that was not admitted"
        );
    }

    // -----------------------------------------------------------------
    // Retention and compaction (AC 3)
    // -----------------------------------------------------------------

    #[test]
    fn the_file_never_holds_more_than_the_maximum() {
        let path = temp_path("ceiling");
        for i in 0..(MAX_OBSERVATIONS as u64 + 10) {
            record(&path, observation(1_000_000 + i * HOUR, 1)).expect("record");
        }
        let held = collected(&path);
        assert_eq!(held.len(), MAX_OBSERVATIONS);
        assert_eq!(
            held.last().expect("newest").at_unix_secs,
            1_000_000 + (MAX_OBSERVATIONS as u64 + 9) * HOUR,
            "the newest observation was the one dropped"
        );
    }

    #[test]
    fn observations_outside_the_window_are_dropped_on_write() {
        let path = temp_path("window");
        record(&path, observation(1_000_000, 1)).expect("record");
        let (_, held) =
            record(&path, observation(1_000_000 + RETENTION_WINDOW_SECS + 1, 1)).expect("record");
        assert_eq!(
            held.len(),
            1,
            "a record older than the retention window survived a write"
        );
        assert_eq!(held[0].at_unix_secs, 1_000_000 + RETENTION_WINDOW_SECS + 1);
    }

    /// Age before count. A burst of recent records must not evict something
    /// still inside the window while a stale record survives on recency.
    #[test]
    fn compaction_drops_stale_records_before_it_drops_old_ones() {
        let mut observations = vec![observation(0, 1)];
        for i in 0..MAX_OBSERVATIONS as u64 {
            observations.push(observation(RETENTION_WINDOW_SECS + i, 1));
        }
        compact(
            &mut observations,
            RETENTION_WINDOW_SECS + MAX_OBSERVATIONS as u64,
        );
        assert_eq!(observations.len(), MAX_OBSERVATIONS);
        assert!(
            observations.iter().all(|o| o.at_unix_secs > 0),
            "the stale record outlived a record inside the window"
        );
    }

    // -----------------------------------------------------------------
    // Round trip
    // -----------------------------------------------------------------

    /// Every field the classifier reads has to survive the file, and a stored
    /// shape that quietly stopped carrying one would make every classification
    /// test pass against in-memory values it never persisted.
    #[test]
    fn the_stored_shape_carries_every_field_the_classifier_reads() {
        let path = temp_path("roundtrip");
        let original = WorkspaceObservation {
            at_unix_secs: 1_700_000_000,
            repositories: vec![
                RepositoryObservation {
                    repository: LocalAlias::of_name("alpha"),
                    worktree_count: 4,
                    linked_worktree_count: 3,
                    detached_worktree_count: 1,
                    single_checkout_branch: None,
                },
                RepositoryObservation {
                    repository: LocalAlias::of_name("beta"),
                    worktree_count: 1,
                    linked_worktree_count: 0,
                    detached_worktree_count: 0,
                    single_checkout_branch: Some(LocalAlias::of_name("trunk")),
                },
            ],
        };
        record(&path, original.clone()).expect("record");
        assert_eq!(collected(&path), vec![original]);
    }

    /// An out-of-order file — hand-edited, or two writes interleaved — must not
    /// make the spacing check compare against the wrong record.
    #[test]
    fn a_file_whose_records_are_out_of_order_is_read_oldest_first() {
        let path = temp_path("unordered");
        let stored = StoredState {
            version: FORMAT_VERSION,
            observations: vec![
                StoredObservation::from(&observation(3_000_000, 1)),
                StoredObservation::from(&observation(1_000_000, 1)),
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&stored).expect("serialize")).expect("write");
        let held = collected(&path);
        assert_eq!(held[0].at_unix_secs, 1_000_000);
        assert_eq!(held[1].at_unix_secs, 3_000_000);
    }

    /// An unreadable store is replaced rather than being a permanent dead end,
    /// and the caller is handed the empty starting point rather than being
    /// allowed to believe the replaced records were evidence.
    #[test]
    fn an_unreadable_store_is_replaced_and_starts_from_nothing() {
        let path = temp_path("replaced");
        std::fs::write(&path, b"garbage").expect("write");
        let (admission, held) = record(&path, observation(1_000_000, 1)).expect("record");
        assert_eq!(admission, Admission::Admitted);
        assert_eq!(held.len(), 1);
        assert_eq!(collected(&path).len(), 1);
    }

    #[test]
    fn the_store_is_created_along_with_its_directory() {
        let path = temp_path("nested").join("a").join("workflow-history.json");
        record(&path, observation(1_000_000, 1)).expect("record");
        assert!(path.exists());
    }

    /// The path is derived, not guessed, and it sits beside the other
    /// per-user state rather than anywhere a user would not think to look.
    #[test]
    fn the_default_path_is_under_application_support() {
        let previous = std::env::var("HOME").ok();
        // SAFETY: single-threaded assertion on a process-wide variable; the
        // previous value is restored below. Matches how
        // `default_settings_path` is tested.
        unsafe { std::env::set_var("HOME", "/tmp/glomeris-h1547-home") };
        let path = default_history_path().expect("a path");
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/glomeris-h1547-home/Library/Application Support/Glomeris/workflow-history.json"
            )
        );
        match previous {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
