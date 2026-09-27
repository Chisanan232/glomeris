//! Persistence for the current pressure episode (HORO-1508).
//!
//! Three separate processes touch this state and none of them is long-lived
//! enough to hold it in memory: the monitor daemon writes it each poll, the
//! menu-bar app reads it to decide whether to raise a notification, and
//! `glomeris pressure respond` writes the user's answer back. So it is a file,
//! and it has to survive a restart of any of the three.
//!
//! ## Why JSON and not the `key = value` settings format
//!
//! [`crate::settings::store`] uses a line format because a human edits it. This
//! file is machinery — nobody hand-writes an episode — and it holds a nested
//! optional record rather than two scalars, so JSON via `serde` is both the
//! smaller amount of code and the one that cannot silently lose a field. Same
//! choice, for the same reason, as
//! [`crate::monitor::persistence::Heartbeat`].
//!
//! ## What a broken or missing file means
//!
//! Both degrade to "no episode open", which is the *forgetful* direction rather
//! than the noisy one, and that asymmetry is deliberate. Getting it wrong the
//! other way — treating an unreadable file as "an episode is open and was
//! already answered" — would silence the notification this ticket exists to
//! raise, and a user whose disk is full would be told nothing at all. Forgetting
//! instead means the next poll above the threshold opens a fresh episode and
//! notifies once. The cost is one notification the user may have already
//! dismissed; the cost of the opposite is silence about a full disk.
//!
//! Note what that implies and what it does not: a lost episode cannot *widen*
//! anything. An episode grants no authority — it decides when the user is
//! spoken to, never what may be deleted — so unlike
//! [`crate::autopilot::store`], where a defaulted field could enlarge an
//! envelope, the worst outcome here is a redundant notification. That is the
//! whole reason this file is allowed to degrade at all.
//!
//! ## Format
//!
//! ```text
//! {"version":1,"next_episode_id":4,"current":{...}}
//! ```
//!
//! `version` is checked on read and a file from a future build is treated as
//! unreadable — i.e. forgotten — rather than best-effort parsed, for the same
//! reason as the settings store: a later version could change what a field
//! *means*, and honouring it under this build's rules would suppress or raise a
//! notification on the strength of a misreading.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::episode::{EpisodeConfig, EpisodeTracker, PressureEpisode};

/// The only format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// The whole of what is persisted between polls.
///
/// `next_episode_id` is stored alongside the episode because episode ids are
/// the notification's identity: Notification Center coalesces two
/// notifications sharing an identifier, so a daemon that restarted and began
/// numbering from 1 again would have its second episode's notification
/// silently replace the first's instead of appearing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeState {
    pub version: u32,
    pub next_episode_id: u64,
    /// `None` when no episode is open, which is also what a first run and a
    /// forgotten file look like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<PressureEpisode>,
}

impl EpisodeState {
    /// Snapshot a tracker for writing.
    pub fn from_tracker(tracker: &EpisodeTracker) -> Self {
        Self {
            version: FORMAT_VERSION,
            next_episode_id: tracker.next_episode_id(),
            current: tracker.current().copied(),
        }
    }

    /// Rebuild a tracker, applying `config` — which comes from the user's
    /// settings *now*, not from whatever it was when the file was written.
    /// A threshold changed while the daemon was down therefore takes effect on
    /// the next poll, and [`EpisodeTracker::reconfigure`]'s rules decide what
    /// that does to the open episode.
    pub fn into_tracker(self, config: EpisodeConfig) -> EpisodeTracker {
        EpisodeTracker::restored(config, self.current, self.next_episode_id)
    }
}

/// Resolves the per-user episode state path — the same
/// `~/Library/Application Support/Glomeris/` directory as the heartbeat and
/// the settings file, and the same missing-`HOME` handling as
/// [`crate::settings::default_settings_path`].
pub fn default_episode_state_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("pressure-episode.json"))
}

/// Reads the episode state at `path`, or `None` if there is nothing usable
/// there.
///
/// Returns `None` rather than an error for every failure — absent, unreadable,
/// malformed, or a version this build does not know. See the module docs for
/// why forgetting is the safe direction. Callers that want to *report* a
/// problem still can: the file's absence and its unreadability are
/// distinguishable by asking the filesystem, and nothing in the poll loop needs
/// to.
pub fn read_episode_state(path: &Path) -> Option<EpisodeState> {
    let contents = std::fs::read_to_string(path).ok()?;
    let state: EpisodeState = serde_json::from_str(&contents).ok()?;
    if state.version != FORMAT_VERSION {
        return None;
    }
    Some(state)
}

/// Loads a tracker from `path`, falling back to a fresh one.
///
/// The single entry point callers should use, so that "what a missing file
/// means" is decided in one place instead of at each of the three processes
/// that read this.
pub fn load_tracker_at(path: &Path, config: EpisodeConfig) -> EpisodeTracker {
    match read_episode_state(path) {
        Some(state) => state.into_tracker(config),
        None => EpisodeTracker::new(config),
    }
}

/// Writes `tracker`'s state to `path`, creating parent directories as needed.
///
/// Through [`crate::atomic_write::write_atomically`] for the reason
/// [`crate::monitor::persistence::write_heartbeat`] documents at length: the
/// app reads this file from another process while the daemon writes it, and a
/// truncate-then-write would let it observe an empty file — which
/// [`read_episode_state`] degrades to "no episode", i.e. a spurious extra
/// notification, for as long as the write took.
///
/// Serialized into memory first so a serialization failure cannot leave a
/// half-written document for the rename to publish.
pub fn save_tracker_at(path: &Path, tracker: &EpisodeTracker) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec(&EpisodeState::from_tracker(tracker)).map_err(io::Error::from)?;
    crate::atomic_write::write_atomically(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::episode::EpisodeResponse;

    const GIB: u64 = 1_073_741_824;

    fn unique_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-episode-test-{tag}-{}-{}",
            std::process::id(),
            crate::monitor::persistence::unix_now_secs()
        ))
    }

    fn config() -> EpisodeConfig {
        EpisodeConfig::new(75.0)
    }

    #[test]
    fn a_tracker_round_trips_through_the_file() {
        let path = unique_path("round-trip");
        let mut tracker = EpisodeTracker::new(config());
        tracker.observe(88.0, 4 * GIB, 1_000);
        tracker.mark_notified(1_010);
        tracker
            .respond(EpisodeResponse::RemindLater, 1_020)
            .unwrap();

        save_tracker_at(&path, &tracker).unwrap();
        let restored = load_tracker_at(&path, config());

        assert_eq!(restored.current(), tracker.current());
        assert_eq!(restored.next_episode_id(), tracker.next_episode_id());
        let episode = restored.current().unwrap();
        assert_eq!(episode.response, Some(EpisodeResponse::RemindLater));
        assert_eq!(episode.notifications_raised, 1);
        assert!(!episode.notification_due);

        let _ = std::fs::remove_file(&path);
    }

    /// The point of persisting at all: a restart must not re-ask a question the
    /// user has already answered.
    #[test]
    fn a_restart_does_not_re_notify_an_answered_episode() {
        let path = unique_path("no-re-notify");
        let mut tracker = EpisodeTracker::new(config());
        tracker.observe(88.0, 4 * GIB, 1_000);
        tracker.mark_notified(1_010);
        tracker
            .respond(EpisodeResponse::IgnoreEpisode, 1_020)
            .unwrap();
        save_tracker_at(&path, &tracker).unwrap();

        let mut restarted = load_tracker_at(&path, config());
        for i in 0..20 {
            let outcome = restarted.observe(88.0, 4 * GIB, 2_000 + i);
            assert_eq!(outcome.opened, None);
            assert!(!outcome.notification_became_due, "re-notified at poll {i}");
        }
        assert!(!restarted.mark_notified(3_000));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_closed_episode_is_persisted_as_no_episode() {
        let path = unique_path("closed");
        let mut tracker = EpisodeTracker::new(config());
        tracker.observe(88.0, 4 * GIB, 1_000);
        tracker.observe(50.0, 60 * GIB, 1_100);
        assert!(tracker.current().is_none());
        save_tracker_at(&path, &tracker).unwrap();

        // Absent from the JSON entirely rather than serialized as null, the
        // same convention as every other optional field in the codebase.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("current"), "{raw}");

        let restored = load_tracker_at(&path, config());
        assert!(restored.current().is_none());
        // But the id counter is still carried, so the next episode is 2.
        assert_eq!(restored.next_episode_id(), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_yields_a_fresh_tracker() {
        let path = unique_path("missing");
        assert!(!path.exists());
        let tracker = load_tracker_at(&path, config());
        assert!(tracker.current().is_none());
        assert_eq!(tracker.next_episode_id(), 1);
    }

    /// Every unusable-file case forgets rather than inventing an episode.
    /// Inventing one would suppress the notification; forgetting one costs at
    /// most a repeat.
    #[test]
    fn unusable_files_are_forgotten_not_guessed_at() {
        let cases = [
            ("empty", String::new()),
            ("not-json", "this is not json".to_string()),
            ("truncated", "{\"version\":1,\"next_epis".to_string()),
            (
                "future-version",
                format!(
                    "{{\"version\":{},\"next_episode_id\":7}}",
                    FORMAT_VERSION + 1
                ),
            ),
            (
                "wrong-shape",
                "{\"version\":1,\"next_episode_id\":\"seven\"}".to_string(),
            ),
        ];
        for (tag, contents) in cases {
            let path = unique_path(tag);
            std::fs::write(&path, &contents).unwrap();
            assert_eq!(read_episode_state(&path), None, "{tag} was parsed");
            let tracker = load_tracker_at(&path, config());
            assert!(tracker.current().is_none(), "{tag} invented an episode");
            // And a fresh tracker still notifies, which is the behaviour the
            // forgetful direction is chosen to preserve.
            let mut tracker = tracker;
            assert!(
                tracker
                    .observe(88.0, 4 * GIB, 1_000)
                    .notification_became_due
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// A version bump must not be silently readable under the old rules. Proven
    /// by writing a document that is valid in every respect *except* its
    /// version, so the version check is the only thing that can reject it.
    #[test]
    fn a_future_version_is_rejected_even_when_otherwise_valid() {
        let path = unique_path("future-valid");
        let mut tracker = EpisodeTracker::new(config());
        tracker.observe(88.0, 4 * GIB, 1_000);
        let mut state = EpisodeState::from_tracker(&tracker);
        // Written at this version it reads back fine...
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(read_episode_state(&path).is_some());
        // ...and the only change is the version.
        state.version = FORMAT_VERSION + 1;
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(read_episode_state(&path), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_creates_the_parent_directory() {
        let dir = unique_path("nested-dir");
        let path = dir.join("deeper").join("pressure-episode.json");
        let tracker = EpisodeTracker::new(config());
        save_tracker_at(&path, &tracker).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader in another process must never observe a partially-written
    /// document. Proven by reading back after every one of a series of writes
    /// whose serialized length changes, which is the case a truncate-then-write
    /// would leave briefly short.
    #[test]
    fn every_intermediate_write_is_readable() {
        let path = unique_path("atomic");
        let mut tracker = EpisodeTracker::new(config());
        for i in 0..10u64 {
            tracker.observe(80.0 + i as f64, (10 - i) * GIB, 1_000 + i);
            if i == 3 {
                tracker.mark_notified(1_000 + i);
            }
            if i == 5 {
                tracker
                    .respond(EpisodeResponse::RemindLater, 1_000 + i)
                    .unwrap();
            }
            save_tracker_at(&path, &tracker).unwrap();
            let state = read_episode_state(&path)
                .unwrap_or_else(|| panic!("write {i} left an unreadable file"));
            assert_eq!(state.current, tracker.current().copied());
        }
        let _ = std::fs::remove_file(&path);
    }

    /// The config is applied from the caller, not restored from the file, so a
    /// threshold changed while the daemon was down governs the next poll.
    #[test]
    fn the_restored_tracker_uses_the_callers_config_not_a_stored_one() {
        let path = unique_path("config-from-caller");
        let mut tracker = EpisodeTracker::new(config());
        tracker.observe(80.0, 5 * GIB, 1_000);
        tracker.mark_notified(1_000);
        save_tracker_at(&path, &tracker).unwrap();

        // Restored under a threshold the open episode no longer breaches: the
        // next observation closes it rather than keeping it alive under a
        // boundary nobody configured.
        let mut restored = load_tracker_at(&path, EpisodeConfig::new(90.0));
        assert_eq!(restored.config().notify_at_used_percent(), 90.0);
        assert_eq!(restored.observe(80.0, 5 * GIB, 1_100).closed, Some(1));

        let _ = std::fs::remove_file(&path);
    }
}
