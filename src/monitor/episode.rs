//! Pressure-episode tracking: when the user should be told, and what their
//! answer means (HORO-1508).
//!
//! ## Why this is not the pressure state machine
//!
//! [`crate::monitor::state_machine::PressureStateMachine`] already debounces
//! the four-state classification and emits at most one transition per
//! confirmed boundary crossing, so the obvious move is to hang the actionable
//! notification off its `Healthy -> Warn` transition and be done. That would
//! be wrong for the reason [`crate::settings`] spells out: the four boundaries
//! describe *the machine* and are deliberately not user-configurable, whereas
//! [`crate::settings::RecoverySettings::notify_at_used_percent`] is a
//! preference about *being told*. Tying the notification to a `WARN` crossing
//! would mean a user who set their alert threshold to 90% used got notified at
//! 75% used anyway, and a user who set it to 60% got nothing until 75%.
//!
//! So an episode is its own axis over the same observations, and
//! `PressureStateMachine` is untouched. The two coexist: the existing
//! four-state `osascript` notification still fires on confirmed transitions,
//! and the episode below is what raises the *actionable* one.
//!
//! ## What an episode is
//!
//! One continuous stretch of the disk being at or above the user's alert
//! threshold. It opens when an observation reaches
//! [`EpisodeConfig::notify_at_used_percent`] and closes only once usage has
//! fallen [`EpisodeConfig::clear_margin_percent`] *below* that — the gap being
//! the documented hysteresis AC1 and AC5 ask for. A disk hovering on the
//! threshold therefore produces one episode, not one per poll, and it takes a
//! real recovery rather than a rounding wobble to end it.
//!
//! At most one notification is raised per episode. The user's answer is
//! recorded on the episode, so it cannot leak into the next one: "ignore this
//! pressure event" silences the episode it was given for and nothing else,
//! which is AC4 by construction rather than by care.
//!
//! ## Why the decision is persisted state rather than a return value
//!
//! [`PressureEpisode::notification_due`] is a field, not something `observe`
//! returns and a caller acts on immediately. Two reasons, and the first is a
//! hard constraint:
//!
//! 1. **The process that notices is not the process that can notify.** An
//!    actionable macOS notification — one carrying buttons — requires
//!    `UNUserNotificationCenter`, which requires an app bundle. The monitor is
//!    a bare launchd process, and the only button-less alternative is the
//!    `display notification` AppleScript this module deliberately leaves
//!    alone. So the daemon records that a notification is *due* and the app
//!    bundle raises it; `mark_notified` is how the app reports back. Rust
//!    still owns the entire policy — whether to notify, what the response
//!    means, when hysteresis clears — and Swift owns only delivery.
//! 2. **A restart must not re-ask a question the user already answered.**
//!    Because the flag lives with the episode and the episode is persisted
//!    (see [`super::episode_store`]), a daemon that restarts mid-episode picks
//!    up a notification already raised and a response already given instead of
//!    starting the episode over. A transient return value would lose both.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How far below the alert threshold usage must fall before the episode is
/// considered over.
///
/// Three points rather than zero, because a volume sitting exactly on the
/// user's threshold crosses it repeatedly as ordinary writes come and go, and
/// a zero-width boundary would end the episode and open a new one — with a new
/// notification — every time it did. Three rather than ten, because the gap is
/// also the distance a recovery has to achieve before Glomeris will agree the
/// problem went away, and a wide gap would keep an episode nominally open long
/// after the disk was comfortable again.
pub const DEFAULT_CLEAR_MARGIN_PERCENT: f64 = 3.0;

/// How long "remind me later" defers for.
///
/// Two hours: long enough that the answer is a real reprieve rather than a
/// snooze button that reappears before the user has finished what they were
/// doing, short enough that a disk which is genuinely filling gets raised
/// again within the same working session. The four-state pressure path is
/// untouched by a snooze, so a disk that deteriorates from WARN to EMERGENCY
/// during one still says so through the channel that has always said it.
pub const DEFAULT_SNOOZE: Duration = Duration::from_secs(2 * 60 * 60);

/// What the user chose when told about a pressure episode.
///
/// A closed enum with no free-form arm, so the app cannot report a response
/// this side has no rule for — the same reason
/// [`crate::settings::SettingsRejection`] has a `GoalRefused` catch-all
/// instead of letting callers invent wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeResponse {
    /// Take me to Recovery. Recorded rather than merely acted on, so that a
    /// user who opened Recovery and then closed it without running anything is
    /// not asked again about the same episode.
    ReviewAndRecover,
    /// Ask me again later. Grants exactly one further notification, once
    /// [`PressureEpisode::snoozed_until_unix_secs`] has passed.
    RemindLater,
    /// Stop telling me about *this* episode. Grants none — and, because it
    /// lives on the episode, expires with it.
    IgnoreEpisode,
}

impl EpisodeResponse {
    /// Stable machine tag, the sole producer of these strings.
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs the set against
    /// the app's wording, same convention as
    /// [`crate::settings::SettingsRejection::as_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReviewAndRecover => "review_and_recover",
            Self::RemindLater => "remind_later",
            Self::IgnoreEpisode => "ignore_episode",
        }
    }

    /// Parses the tag [`Self::as_str`] produces, for the CLI's
    /// `pressure respond --action` argument.
    ///
    /// Accepts the hyphenated spelling too, because that is what a command
    /// line looks like (`--action review-and-recover`) and refusing it would
    /// be a papercut with no safety value. Nothing else is accepted: an
    /// unrecognized action is refused rather than mapped onto the nearest
    /// match, since the nearest match to a misspelled "ignore" could be the
    /// one that starts a recovery.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().replace('-', "_").as_str() {
            "review_and_recover" => Some(Self::ReviewAndRecover),
            "remind_later" => Some(Self::RemindLater),
            "ignore_episode" => Some(Self::IgnoreEpisode),
            _ => None,
        }
    }

    /// Every variant, for the CLI's usage text and for tests that must cover
    /// the whole enum rather than the arms someone remembered.
    pub const ALL: [Self; 3] = [
        Self::ReviewAndRecover,
        Self::RemindLater,
        Self::IgnoreEpisode,
    ];
}

/// The thresholds and durations an [`EpisodeTracker`] works to.
///
/// `notify_at_used_percent` comes from the user's settings; the other two are
/// the constants above, overridable only so tests can drive a snooze without
/// waiting two hours. Deliberately *not* user-configurable: the product has
/// two percentage settings already and the whole of HORO-1507's reasoning is
/// that a third number about the same disk would be a worse product than two
/// clear ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpisodeConfig {
    notify_at_used_percent: f64,
    clear_margin_percent: f64,
    snooze: Duration,
}

impl EpisodeConfig {
    /// Config for a given alert threshold, with the default margin and snooze.
    pub fn new(notify_at_used_percent: f64) -> Self {
        Self {
            notify_at_used_percent,
            clear_margin_percent: DEFAULT_CLEAR_MARGIN_PERCENT,
            snooze: DEFAULT_SNOOZE,
        }
    }

    /// Test-only seam for the hysteresis width.
    ///
    /// Must stay positive. A zero margin would put the open boundary (`>=`
    /// threshold) and the close boundary (`<=` clear) on the same number, and
    /// since [`EpisodeTracker::observe`] checks closing first, an observation
    /// exactly at the threshold would close an episode and then decline to open
    /// one — a disk pinned at precisely the alert threshold would be silently
    /// unmonitored.
    pub fn with_clear_margin_percent(mut self, percent: f64) -> Self {
        debug_assert!(
            percent > 0.0,
            "the hysteresis margin must be positive so the open and close boundaries are disjoint"
        );
        self.clear_margin_percent = percent;
        self
    }

    /// Test-only seam for the snooze duration.
    pub fn with_snooze(mut self, snooze: Duration) -> Self {
        self.snooze = snooze;
        self
    }

    pub fn notify_at_used_percent(&self) -> f64 {
        self.notify_at_used_percent
    }

    pub fn clear_margin_percent(&self) -> f64 {
        self.clear_margin_percent
    }

    pub fn snooze(&self) -> Duration {
        self.snooze
    }

    /// The usage an episode closes at or below.
    ///
    /// Floored at zero for a threshold set lower than the margin, so the
    /// boundary is always a percentage a reading can actually reach. Note that
    /// the floor alone is not sufficient — [`EpisodeTracker::observe`] closes on
    /// `<=` this value rather than `<`, because a completely empty disk reads
    /// 0% used and a strict comparison against a floored boundary of 0 would
    /// never fire. That combination is what stops a low threshold from opening
    /// one episode that never ends and therefore silences every episode after
    /// it.
    pub fn clear_at_used_percent(&self) -> f64 {
        (self.notify_at_used_percent - self.clear_margin_percent).max(0.0)
    }
}

/// One continuous stretch of the disk being at or above the alert threshold.
///
/// Serialized as-is into the episode state file. Public fields, because this
/// is a *record* of what happened rather than a type that grants authority:
/// every rule about it lives in [`EpisodeTracker`], and nothing an
/// episode says can widen what Glomeris is permitted to delete — the worst a
/// forged episode achieves is a notification the user did not need, which is
/// why this type does not need the private-fields-and-one-constructor
/// treatment [`crate::settings::RecoverySettings`] gets.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PressureEpisode {
    /// Monotonically increasing, never reused, and carried across daemon
    /// restarts by the state file. The app uses it as the notification's
    /// identifier, so two notifications about one episode coalesce and two
    /// episodes never do.
    pub episode_id: u64,
    pub opened_unix_secs: u64,
    /// Usage at the observation that opened the episode.
    pub opened_used_percent: f64,
    /// Worst usage seen while it has been open. Reported so the user is told
    /// how bad it got, not merely how bad it is now — a disk that filled and
    /// was partially recovered is a different story from one that never moved.
    pub peak_used_percent: f64,
    /// Most recent observation.
    pub latest_used_percent: f64,
    pub latest_free_bytes: u64,
    pub latest_unix_secs: u64,
    /// Set when a notification should be raised and not yet has been. The app
    /// clears it through [`EpisodeTracker::mark_notified`].
    pub notification_due: bool,
    /// How many have actually been raised. More than one means a snooze
    /// elapsed; it can never grow on its own.
    pub notifications_raised: u32,
    pub last_notified_unix_secs: Option<u64>,
    pub response: Option<EpisodeResponse>,
    pub responded_unix_secs: Option<u64>,
    /// When a [`EpisodeResponse::RemindLater`] stops deferring. Absent for
    /// every other response, so "is a snooze pending" is a question about this
    /// field alone.
    pub snoozed_until_unix_secs: Option<u64>,
}

impl PressureEpisode {
    fn open(episode_id: u64, used_percent: f64, free_bytes: u64, now: u64) -> Self {
        Self {
            episode_id,
            opened_unix_secs: now,
            opened_used_percent: used_percent,
            peak_used_percent: used_percent,
            latest_used_percent: used_percent,
            latest_free_bytes: free_bytes,
            latest_unix_secs: now,
            // An episode opens *because* the user asked to be told at this
            // usage, so the notification is due from the first observation.
            notification_due: true,
            notifications_raised: 0,
            last_notified_unix_secs: None,
            response: None,
            responded_unix_secs: None,
            snoozed_until_unix_secs: None,
        }
    }

    /// Whether a snooze is pending and not yet elapsed at `now`.
    pub fn is_snoozed_at(&self, now: u64) -> bool {
        matches!(self.snoozed_until_unix_secs, Some(until) if now < until)
    }
}

/// What one observation did to the episode state.
///
/// Returned so a caller can log or assert on the transition without diffing
/// two snapshots of the tracker — the same reason
/// [`crate::monitor::PollOutcome`] exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EpisodeOutcome {
    /// Id of an episode this observation opened.
    pub opened: Option<u64>,
    /// Id of an episode this observation closed, because usage fell below the
    /// hysteresis boundary.
    pub closed: Option<u64>,
    /// Set when this observation is what made a notification due — either by
    /// opening an episode or by a snooze elapsing. Distinct from the episode's
    /// own `notification_due`, which stays set until the app raises it.
    pub notification_became_due: bool,
    /// Set when this observation ended a snooze. Implies
    /// `notification_became_due`.
    pub snooze_elapsed: bool,
}

/// Tracks the current episode across observations.
///
/// Holds no I/O and no clock: `now` is passed in, exactly as
/// [`PressureStateMachine`](super::state_machine::PressureStateMachine) takes
/// an already-classified state, so every rule below is testable without a
/// filesystem or a sleep.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeTracker {
    config: EpisodeConfig,
    current: Option<PressureEpisode>,
    next_episode_id: u64,
}

impl EpisodeTracker {
    /// A tracker with no episode open and ids starting at 1.
    pub fn new(config: EpisodeConfig) -> Self {
        Self {
            config,
            current: None,
            next_episode_id: 1,
        }
    }

    /// Restores a tracker from persisted state.
    ///
    /// `next_episode_id` is forced past any restored episode's own id, because
    /// a state file that disagreed with itself would otherwise mint a second
    /// episode sharing the first one's notification identifier — and identical
    /// identifiers coalesce in Notification Center, so the second episode's
    /// notification would silently replace the first's rather than appear.
    pub fn restored(
        config: EpisodeConfig,
        current: Option<PressureEpisode>,
        next_episode_id: u64,
    ) -> Self {
        let floor = current.map(|e| e.episode_id + 1).unwrap_or(1);
        Self {
            config,
            current,
            next_episode_id: next_episode_id.max(floor).max(1),
        }
    }

    pub fn config(&self) -> &EpisodeConfig {
        &self.config
    }

    /// The open episode, if any.
    pub fn current(&self) -> Option<&PressureEpisode> {
        self.current.as_ref()
    }

    pub fn next_episode_id(&self) -> u64 {
        self.next_episode_id
    }

    /// Replaces the config, keeping the open episode.
    ///
    /// This is what a settings change looks like to an in-flight episode: the
    /// user moved their threshold while the disk was already above the old one.
    /// The episode is deliberately *not* reopened or re-notified — it is the
    /// same continuous stretch of disk pressure, and a preference change is not
    /// a new event about the disk. The new boundary governs from here: raising
    /// the threshold above current usage closes the episode on the next
    /// observation, which is the honest reading of "stop telling me until it's
    /// worse than this".
    pub fn reconfigure(&mut self, config: EpisodeConfig) {
        self.config = config;
    }

    /// Feed one observation.
    ///
    /// The whole decision, in the order the rules apply:
    ///
    /// 1. At or below the clear boundary: any open episode closes. Nothing is
    ///    notified — a disk that recovered is not news.
    /// 2. No episode open and at or above the threshold: open one, notification
    ///    due.
    /// 3. Episode open: update the readings; if a snooze has elapsed, make one
    ///    further notification due and consume the snooze.
    ///
    /// Both boundaries are inclusive, and they are disjoint because the margin
    /// is positive (see
    /// [`EpisodeConfig::with_clear_margin_percent`]). An observation between
    /// them — below the threshold but above the clear boundary — neither opens
    /// nor closes anything. That band is the hysteresis, and it is why a volume
    /// oscillating around the threshold produces one episode rather than a
    /// notification per poll.
    pub fn observe(&mut self, used_percent: f64, free_bytes: u64, now: u64) -> EpisodeOutcome {
        let mut outcome = EpisodeOutcome::default();

        // A non-finite reading is not evidence about the disk, so it must not
        // be allowed to open an episode (claiming pressure that was never
        // measured) or close one (claiming a recovery that never happened).
        // Ignored entirely, which leaves whatever the last real observation
        // established standing.
        if !used_percent.is_finite() {
            return outcome;
        }

        if used_percent <= self.config.clear_at_used_percent() {
            if let Some(closed) = self.current.take() {
                outcome.closed = Some(closed.episode_id);
            }
            return outcome;
        }

        match self.current.as_mut() {
            None => {
                if used_percent >= self.config.notify_at_used_percent {
                    let id = self.next_episode_id;
                    self.next_episode_id += 1;
                    self.current = Some(PressureEpisode::open(id, used_percent, free_bytes, now));
                    outcome.opened = Some(id);
                    outcome.notification_became_due = true;
                }
            }
            Some(episode) => {
                episode.latest_used_percent = used_percent;
                episode.latest_free_bytes = free_bytes;
                episode.latest_unix_secs = now;
                if used_percent > episode.peak_used_percent {
                    episode.peak_used_percent = used_percent;
                }

                if let Some(until) = episode.snoozed_until_unix_secs {
                    if now >= until {
                        // The snooze is consumed as it fires, so it grants
                        // exactly one further notification. Leaving it set
                        // would make every subsequent poll re-raise it, which
                        // is the storm AC5 forbids; clearing the response too
                        // means the episode is back to awaiting an answer
                        // rather than remembering one it has already honoured.
                        episode.snoozed_until_unix_secs = None;
                        episode.response = None;
                        episode.responded_unix_secs = None;
                        episode.notification_due = true;
                        outcome.notification_became_due = true;
                        outcome.snooze_elapsed = true;
                    }
                }
            }
        }

        outcome
    }

    /// Records that the app raised the due notification.
    ///
    /// Refuses when there was nothing to raise, so a caller that reports
    /// success has to have been told so. Idempotent in the safe direction:
    /// calling it twice clears a flag that is already clear and refuses the
    /// second time, rather than crediting a second notification that was never
    /// shown.
    ///
    /// The two ways it can refuse — no episode at all, and an episode whose
    /// notification is not owed — are one rejection deliberately. From the
    /// app's side they are the same fact: there was nothing to acknowledge.
    pub fn mark_notified(&mut self, now: u64) -> Result<(), EpisodeRejection> {
        match self.current.as_mut() {
            Some(episode) if episode.notification_due => {
                episode.notification_due = false;
                episode.notifications_raised += 1;
                episode.last_notified_unix_secs = Some(now);
                Ok(())
            }
            _ => Err(EpisodeRejection::NoNotificationDue),
        }
    }

    /// Records the user's answer to the current episode.
    ///
    /// Overwriting an existing answer is allowed on purpose: a user who
    /// ignored an episode and then thought better of it should be able to open
    /// Recovery for it, and the alternative — refusing — would leave the app
    /// holding a response it cannot act on. What no answer can do is outlive
    /// its episode, because it is stored on the episode.
    ///
    /// Clears `notification_due` as well, so answering a notification that the
    /// app has not yet reported raising cannot leave one queued behind the
    /// answer.
    pub fn respond(&mut self, response: EpisodeResponse, now: u64) -> Result<(), EpisodeRejection> {
        let snooze = self.config.snooze.as_secs();
        let Some(episode) = self.current.as_mut() else {
            return Err(EpisodeRejection::NoOpenEpisode);
        };

        episode.response = Some(response);
        episode.responded_unix_secs = Some(now);
        episode.notification_due = false;
        episode.snoozed_until_unix_secs = match response {
            EpisodeResponse::RemindLater => Some(now.saturating_add(snooze)),
            // Explicitly cleared rather than left alone: a user who snoozed
            // and then chose to review or ignore must not have the old
            // deadline fire behind their newer answer.
            EpisodeResponse::ReviewAndRecover | EpisodeResponse::IgnoreEpisode => None,
        };
        Ok(())
    }
}

/// Why an answer or an acknowledgement could not be recorded.
///
/// Both write paths refuse through one type, because both refuse for the same
/// underlying reason — the state the app was told about is no longer the state
/// on disk — and a caller that handled one and not the other would be handling
/// half a race.
///
/// Neither of these is a malfunction. The ordinary cause of both is benign: the
/// disk recovered between the banner appearing and the button being pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeRejection {
    /// There is no episode to respond to — the disk is below the threshold, or
    /// the episode ended between the notification being shown and the button
    /// being pressed. Reported rather than silently accepted, because an
    /// answer stored against nothing would be an answer that never applies to
    /// anything.
    NoOpenEpisode,
    /// There was no notification owed to acknowledge: either no episode is
    /// open, or its notification was already raised. Reported rather than
    /// counted, so `notifications_raised` can only ever mean "banners the user
    /// could have seen" — the figure AC5 is checked against.
    NoNotificationDue,
}

impl std::fmt::Display for EpisodeRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOpenEpisode => write!(
                f,
                "there is no open pressure episode to respond to \
                 (disk usage is below the alert threshold)"
            ),
            Self::NoNotificationDue => write!(
                f,
                "no pressure notification is owed \
                 (there is no open episode, or its notification was already raised)"
            ),
        }
    }
}

impl std::error::Error for EpisodeRejection {}

impl EpisodeRejection {
    /// Stable machine tag, sole producer.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoOpenEpisode => "no_open_episode",
            Self::NoNotificationDue => "no_notification_due",
        }
    }

    /// Every variant, for the vocabulary test and for a client that wants to
    /// enumerate what it must handle.
    pub const ALL: [Self; 2] = [Self::NoOpenEpisode, Self::NoNotificationDue];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 75% used threshold with the default 3-point margin, so the clear
    /// boundary is 72% used.
    fn tracker() -> EpisodeTracker {
        EpisodeTracker::new(EpisodeConfig::new(75.0))
    }

    const GIB: u64 = 1_073_741_824;

    #[test]
    fn crossing_the_threshold_opens_one_episode_and_makes_a_notification_due() {
        let mut t = tracker();
        let outcome = t.observe(75.0, 10 * GIB, 1_000);
        assert_eq!(outcome.opened, Some(1));
        assert!(outcome.notification_became_due);
        let episode = t.current().expect("an episode must be open");
        assert_eq!(episode.episode_id, 1);
        assert!(episode.notification_due);
        assert_eq!(episode.notifications_raised, 0);
        assert_eq!(episode.opened_used_percent, 75.0);
    }

    #[test]
    fn staying_above_the_threshold_never_opens_a_second_episode() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        for i in 0..50 {
            let outcome = t.observe(80.0 + (i % 3) as f64, 5 * GIB, 1_100 + i);
            assert_eq!(outcome.opened, None, "poll {i} opened a second episode");
            assert!(!outcome.notification_became_due, "poll {i} re-notified");
        }
        assert_eq!(t.current().unwrap().notifications_raised, 1);
    }

    /// AC5, at the boundary that actually matters: a volume whose usage
    /// wobbles across the threshold itself. Without the hysteresis band each
    /// dip would close the episode and each rise would open a new one, which
    /// is a notification per poll.
    #[test]
    fn oscillating_across_the_threshold_yields_exactly_one_notification() {
        let mut t = tracker();
        let mut raised = 0;
        for i in 0..40 {
            // 74.5 and 75.5 straddle the 75% threshold and both sit above the
            // 72% clear boundary.
            let used = if i % 2 == 0 { 75.5 } else { 74.5 };
            t.observe(used, 5 * GIB, 1_000 + i);
            if t.mark_notified(1_000 + i).is_ok() {
                raised += 1;
            }
        }
        assert_eq!(raised, 1, "one notification for one continuous episode");
        assert_eq!(t.current().unwrap().episode_id, 1);
    }

    #[test]
    fn falling_below_the_clear_boundary_closes_the_episode() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        // Below the threshold but inside the hysteresis band: still open.
        let outcome = t.observe(73.0, 6 * GIB, 1_100);
        assert_eq!(outcome.closed, None);
        assert!(t.current().is_some());
        // Below the clear boundary: closed.
        let outcome = t.observe(71.9, 8 * GIB, 1_200);
        assert_eq!(outcome.closed, Some(1));
        assert!(t.current().is_none());
    }

    /// AC7's re-crossing case. A genuinely new episode gets a new id and its
    /// own notification — the whole point of closing the old one.
    #[test]
    fn recovering_and_re_crossing_opens_a_new_episode_with_a_new_id() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        assert!(t.mark_notified(1_000).is_ok());
        t.observe(60.0, 40 * GIB, 2_000);
        assert!(t.current().is_none());

        let outcome = t.observe(80.0, 5 * GIB, 3_000);
        assert_eq!(outcome.opened, Some(2), "a new episode, not the old one");
        assert!(outcome.notification_became_due);
        assert!(t.mark_notified(3_000).is_ok());
        assert_eq!(t.current().unwrap().notifications_raised, 1);
    }

    #[test]
    fn a_response_suppresses_further_notification_within_the_episode() {
        for response in [
            EpisodeResponse::ReviewAndRecover,
            EpisodeResponse::IgnoreEpisode,
        ] {
            let mut t = tracker();
            t.observe(80.0, 5 * GIB, 1_000);
            t.mark_notified(1_000).unwrap();
            t.respond(response, 1_050).unwrap();
            for i in 0..100 {
                let outcome = t.observe(85.0, 2 * GIB, 1_100 + i);
                assert!(
                    !outcome.notification_became_due,
                    "{response:?} was re-notified at poll {i}"
                );
            }
            assert_eq!(t.current().unwrap().notifications_raised, 1);
        }
    }

    /// AC4. "Ignore this pressure event" is scoped to the event, and the proof
    /// is that the *next* episode still notifies.
    #[test]
    fn ignoring_an_episode_cannot_silence_the_next_one() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::IgnoreEpisode, 1_050).unwrap();

        // Recover below the clear boundary, then fill up again.
        t.observe(50.0, 60 * GIB, 2_000);
        let outcome = t.observe(80.0, 5 * GIB, 3_000);
        assert_eq!(outcome.opened, Some(2));
        assert!(
            outcome.notification_became_due,
            "ignoring one episode must not disable monitoring"
        );
        assert!(t.mark_notified(3_000).is_ok());
        assert_eq!(t.current().unwrap().response, None);
    }

    /// AC3. The deadline is `now + snooze` exactly, nothing fires before it,
    /// and exactly one notification fires after.
    #[test]
    fn remind_later_is_deterministic_and_fires_exactly_once() {
        let mut t =
            EpisodeTracker::new(EpisodeConfig::new(75.0).with_snooze(Duration::from_secs(600)));
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::RemindLater, 1_000).unwrap();
        assert_eq!(
            t.current().unwrap().snoozed_until_unix_secs,
            Some(1_600),
            "the deadline is now + snooze, with no jitter"
        );

        // One second before the deadline: nothing.
        let outcome = t.observe(80.0, 5 * GIB, 1_599);
        assert!(!outcome.notification_became_due);
        assert!(t.current().unwrap().is_snoozed_at(1_599));

        // At the deadline: exactly one.
        let outcome = t.observe(80.0, 5 * GIB, 1_600);
        assert!(outcome.snooze_elapsed);
        assert!(outcome.notification_became_due);
        assert!(t.mark_notified(1_600).is_ok());
        assert_eq!(t.current().unwrap().notifications_raised, 2);

        // And never again without another snooze.
        for i in 0..100 {
            let outcome = t.observe(80.0, 5 * GIB, 1_700 + i);
            assert!(!outcome.notification_became_due, "re-fired at poll {i}");
        }
    }

    #[test]
    fn snoozing_twice_grants_exactly_one_notification_each_time() {
        let mut t =
            EpisodeTracker::new(EpisodeConfig::new(75.0).with_snooze(Duration::from_secs(100)));
        t.observe(80.0, 5 * GIB, 1_000);
        let mut raised = if t.mark_notified(1_000).is_ok() { 1 } else { 0 };

        for round in 0..2 {
            let at = 1_000 + round * 1_000;
            t.respond(EpisodeResponse::RemindLater, at).unwrap();
            for step in 0..200u64 {
                t.observe(80.0, 5 * GIB, at + step);
                if t.mark_notified(at + step).is_ok() {
                    raised += 1;
                }
            }
        }
        assert_eq!(raised, 3, "one opening notification plus one per snooze");
    }

    #[test]
    fn choosing_review_after_a_snooze_cancels_the_deadline() {
        let mut t =
            EpisodeTracker::new(EpisodeConfig::new(75.0).with_snooze(Duration::from_secs(100)));
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::RemindLater, 1_000).unwrap();
        t.respond(EpisodeResponse::ReviewAndRecover, 1_010).unwrap();
        assert_eq!(t.current().unwrap().snoozed_until_unix_secs, None);
        for i in 0..200 {
            let outcome = t.observe(80.0, 5 * GIB, 1_100 + i);
            assert!(
                !outcome.notification_became_due,
                "a cancelled snooze fired at poll {i}"
            );
        }
    }

    #[test]
    fn a_snooze_does_not_survive_its_episode() {
        let mut t =
            EpisodeTracker::new(EpisodeConfig::new(75.0).with_snooze(Duration::from_secs(100)));
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::RemindLater, 1_000).unwrap();
        // Episode ends while the snooze is still pending.
        t.observe(50.0, 60 * GIB, 1_050);
        assert!(t.current().is_none());
        // A new episode is not born snoozed.
        t.observe(80.0, 5 * GIB, 1_060);
        let episode = t.current().unwrap();
        assert_eq!(episode.snoozed_until_unix_secs, None);
        assert!(episode.notification_due);
    }

    #[test]
    fn responding_with_no_open_episode_is_refused() {
        let mut t = tracker();
        assert_eq!(
            t.respond(EpisodeResponse::ReviewAndRecover, 1_000),
            Err(EpisodeRejection::NoOpenEpisode)
        );
    }

    #[test]
    fn mark_notified_reports_whether_there_was_anything_to_raise() {
        let mut t = tracker();
        assert_eq!(
            t.mark_notified(1_000),
            Err(EpisodeRejection::NoNotificationDue),
            "no episode, nothing due"
        );
        t.observe(80.0, 5 * GIB, 1_000);
        assert!(t.mark_notified(1_000).is_ok());
        assert_eq!(
            t.mark_notified(1_001),
            Err(EpisodeRejection::NoNotificationDue),
            "already raised"
        );
        assert_eq!(t.current().unwrap().notifications_raised, 1);
    }

    /// An acknowledgement that was refused must not be counted. Otherwise
    /// `notifications_raised` would drift above the number of banners the user
    /// could have seen, and it is the figure AC5's "no storms" claim is checked
    /// against.
    #[test]
    fn a_refused_acknowledgement_credits_nothing() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        for i in 0..20 {
            assert!(t.mark_notified(1_100 + i).is_err());
        }
        assert_eq!(t.current().unwrap().notifications_raised, 1);
        assert_eq!(t.current().unwrap().last_notified_unix_secs, Some(1_000));
    }

    /// Both refusal tokens are distinct and non-empty, and `ALL` lists each
    /// exactly once — the same shape the settings vocabulary test asserts, so a
    /// new variant cannot ship without a tag.
    #[test]
    fn rejection_tokens_are_distinct_and_fully_enumerated() {
        let tags: Vec<&str> = EpisodeRejection::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(tags.len(), EpisodeRejection::ALL.len());
        for tag in &tags {
            assert!(!tag.is_empty());
            assert_eq!(*tag, tag.to_lowercase());
        }
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len(), "duplicate tag in {tags:?}");
        // And each one's message says which situation it is, so a user reading
        // it is not left with a token.
        for rejection in EpisodeRejection::ALL {
            let message = rejection.to_string();
            assert!(!message.is_empty(), "{rejection:?}");
            assert!(
                message.contains("episode") || message.contains("notification"),
                "{message}"
            );
        }
    }

    #[test]
    fn peak_usage_records_the_worst_reading_not_the_latest() {
        let mut t = tracker();
        t.observe(76.0, 9 * GIB, 1_000);
        t.observe(94.0, GIB, 1_100);
        t.observe(78.0, 7 * GIB, 1_200);
        let episode = t.current().unwrap();
        assert_eq!(episode.peak_used_percent, 94.0);
        assert_eq!(episode.latest_used_percent, 78.0);
        assert_eq!(episode.latest_free_bytes, 7 * GIB);
        assert_eq!(episode.latest_unix_secs, 1_200);
    }

    /// A `statfs` that answers with a NaN must not be able to invent an
    /// episode or to end one. Both directions matter: the first would notify
    /// about a disk nobody measured, the second would report a recovery that
    /// never happened.
    #[test]
    fn a_non_finite_reading_neither_opens_nor_closes_an_episode() {
        let mut t = tracker();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let outcome = t.observe(bad, 5 * GIB, 1_000);
            assert_eq!(outcome, EpisodeOutcome::default(), "{bad} opened something");
        }
        assert!(t.current().is_none());

        t.observe(80.0, 5 * GIB, 1_000);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let outcome = t.observe(bad, 5 * GIB, 1_100);
            assert_eq!(outcome.closed, None, "{bad} closed an episode");
        }
        assert_eq!(t.current().unwrap().episode_id, 1);
    }

    #[test]
    fn the_clear_boundary_is_the_threshold_less_the_margin_and_never_negative() {
        let config = EpisodeConfig::new(75.0);
        assert_eq!(config.clear_at_used_percent(), 72.0);
        // A threshold below the margin would otherwise produce a negative
        // boundary that no reading can fall under, leaving the first episode
        // open for ever.
        let low = EpisodeConfig::new(1.0);
        assert_eq!(low.clear_at_used_percent(), 0.0);
        let mut t = EpisodeTracker::new(low);
        t.observe(50.0, 5 * GIB, 1_000);
        assert!(t.current().is_some());
        let outcome = t.observe(0.0, 500 * GIB, 1_100);
        assert_eq!(
            outcome.closed,
            Some(1),
            "an empty disk must be able to end an episode at any threshold"
        );
    }

    /// The floored boundary is only half the fix: closing has to be inclusive
    /// too, or a completely empty disk (0% used) never falls *under* a boundary
    /// of 0 and the episode outlives the condition that opened it. Written as
    /// its own test because it is the case the floor alone silently failed.
    #[test]
    fn an_empty_disk_closes_an_episode_even_at_the_floored_boundary() {
        let mut t = EpisodeTracker::new(EpisodeConfig::new(2.0));
        assert_eq!(t.config().clear_at_used_percent(), 0.0);
        t.observe(5.0, GIB, 1_000);
        assert_eq!(t.observe(0.0, 900 * GIB, 1_100).closed, Some(1));
        // And the next crossing is a fresh episode rather than a silence.
        let outcome = t.observe(5.0, GIB, 1_200);
        assert_eq!(outcome.opened, Some(2));
        assert!(outcome.notification_became_due);
    }

    /// Both boundaries are inclusive, so their disjointness rests entirely on
    /// the margin being positive. If they ever coincided, an observation
    /// exactly at the threshold would close an episode and decline to open one
    /// — a disk pinned at the alert threshold would go unmonitored.
    #[test]
    fn the_open_and_close_boundaries_never_coincide() {
        for threshold in [
            crate::settings::MIN_NOTIFY_AT_USED_PERCENT,
            50.0,
            75.0,
            crate::settings::MAX_NOTIFY_AT_USED_PERCENT,
        ] {
            let config = EpisodeConfig::new(threshold);
            assert!(
                config.clear_at_used_percent() < config.notify_at_used_percent(),
                "boundaries coincide at a {threshold}% used threshold"
            );
            // And an observation exactly at the threshold opens an episode
            // rather than being swallowed by the close check that runs first.
            let mut t = EpisodeTracker::new(config);
            assert_eq!(t.observe(threshold, GIB, 1_000).opened, Some(1));
        }
    }

    /// Restoring must never mint an id that collides with the episode being
    /// restored, because the id is the notification's identity and a collision
    /// means the newer notification silently replaces the older.
    #[test]
    fn restoring_forces_the_next_id_past_the_restored_episode() {
        let episode = PressureEpisode::open(9, 80.0, 5 * GIB, 1_000);
        // A state file claiming a next id at or below the restored episode's.
        for claimed in [0, 1, 9] {
            let t = EpisodeTracker::restored(EpisodeConfig::new(75.0), Some(episode), claimed);
            assert_eq!(t.next_episode_id(), 10, "claimed {claimed}");
        }
        // And an honest one is respected.
        let t = EpisodeTracker::restored(EpisodeConfig::new(75.0), Some(episode), 12);
        assert_eq!(t.next_episode_id(), 12);
        // With no episode restored, ids still start at 1 rather than 0.
        let t = EpisodeTracker::restored(EpisodeConfig::new(75.0), None, 0);
        assert_eq!(t.next_episode_id(), 1);
    }

    /// A restart mid-episode must pick up where it left off: no new episode, no
    /// second notification, and the user's answer still honoured.
    #[test]
    fn a_restored_episode_is_not_re_notified() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::IgnoreEpisode, 1_050).unwrap();
        let saved = *t.current().unwrap();
        let next = t.next_episode_id();

        let mut restored = EpisodeTracker::restored(EpisodeConfig::new(75.0), Some(saved), next);
        for i in 0..50 {
            let outcome = restored.observe(80.0, 5 * GIB, 2_000 + i);
            assert_eq!(outcome.opened, None);
            assert!(!outcome.notification_became_due, "re-notified at poll {i}");
        }
        assert_eq!(
            restored.current().unwrap().response,
            Some(EpisodeResponse::IgnoreEpisode)
        );
    }

    /// Raising the threshold above current usage is how a user says "stop
    /// telling me until it is worse than this", so the open episode must end
    /// rather than persist under a boundary it no longer breaches.
    #[test]
    fn reconfiguring_the_threshold_upward_closes_an_episode_it_no_longer_breaches() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.reconfigure(EpisodeConfig::new(90.0));
        let outcome = t.observe(80.0, 5 * GIB, 1_100);
        assert_eq!(outcome.closed, Some(1));
        assert!(t.current().is_none());
    }

    /// A preference change is not an event about the disk, so lowering the
    /// threshold while an episode is already open must not re-raise it.
    #[test]
    fn reconfiguring_does_not_re_notify_an_open_episode() {
        let mut t = tracker();
        t.observe(80.0, 5 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::IgnoreEpisode, 1_010).unwrap();
        t.reconfigure(EpisodeConfig::new(60.0));
        let outcome = t.observe(80.0, 5 * GIB, 1_100);
        assert_eq!(outcome.opened, None);
        assert!(!outcome.notification_became_due);
        assert_eq!(t.current().unwrap().episode_id, 1);
    }

    #[test]
    fn response_tags_round_trip_and_nothing_else_parses() {
        for response in EpisodeResponse::ALL {
            assert_eq!(EpisodeResponse::parse(response.as_str()), Some(response));
            // The hyphenated command-line spelling too.
            assert_eq!(
                EpisodeResponse::parse(&response.as_str().replace('_', "-")),
                Some(response)
            );
        }
        for junk in ["", "skip", "ignore", "review", "ignore_all", "REVIEW"] {
            assert_eq!(EpisodeResponse::parse(junk), None, "{junk} parsed");
        }
    }

    /// The tags are a wire format the app branches on, so they are pinned
    /// literally rather than derived — renaming a variant must not silently
    /// change what the app receives.
    #[test]
    fn response_tags_are_the_documented_strings() {
        assert_eq!(
            EpisodeResponse::ALL.map(|r| r.as_str()),
            ["review_and_recover", "remind_later", "ignore_episode"]
        );
    }

    /// The default snooze must be long enough to be a reprieve and short
    /// enough to come back the same working day, which is the claim its doc
    /// comment makes.
    #[test]
    fn the_default_snooze_is_within_one_working_session() {
        assert!(DEFAULT_SNOOZE >= Duration::from_secs(30 * 60));
        assert!(DEFAULT_SNOOZE <= Duration::from_secs(8 * 60 * 60));
    }
}
