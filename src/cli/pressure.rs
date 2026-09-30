//! Report building for the pressure-episode surface (HORO-1508).
//!
//! The projection layer between [`crate::monitor::episode`] and what
//! `glomeris pressure show|notified|respond` prints, in both prose and
//! `--json`. Same division of labour as [`crate::cli::settings`]: the domain
//! type owns the rules, this module owns shape and wording, and `main.rs` owns
//! exit codes.
//!
//! ## Why this surface exists at all
//!
//! An actionable macOS notification — one with "Review & recover" and "Remind
//! me later" buttons — can only be posted through `UNUserNotificationCenter`,
//! which requires an application bundle. The monitor daemon is a bare launchd
//! process, so **the process that notices disk pressure is not the process
//! that can ask the user about it**. The daemon therefore records that a
//! notification is *owed*, the menu-bar app reads that through
//! `pressure show`, raises the banner, and reports the outcome back through
//! `pressure notified` and `pressure respond`.
//!
//! Every rule stays in Rust: when an episode opens, when hysteresis closes it,
//! how long a snooze lasts, what an answer means, and whether a notification
//! is owed. Swift owns delivery and nothing else. That is deliberate — a
//! client re-deriving "should I notify?" from a threshold and a percentage
//! would get the snooze and already-raised cases wrong, and the symptom would
//! be a notification storm, which is exactly what AC5 forbids.
//!
//! One rule carries over unchanged from [`crate::cli::settings`]: **no
//! ambiguous percentages**. Four percentages can appear on this surface at
//! once — current usage, the alert threshold, the hysteresis clear boundary
//! and the recovery goal — so every field names its axis and every prose line
//! says `used`.

use crate::monitor::episode::{EpisodeRejection, EpisodeResponse, EpisodeTracker, PressureEpisode};
use crate::monitor::fs_stat::FsUsage;
use crate::monitor::ThresholdConfig;
use crate::reporting::dto::{PressureEpisodeReport, PressureRejectionReport, PressureStatusReport};
use crate::reporting::human_bytes;
use crate::reporting::used_percent::{configured_used_percent_figure, used_percent_text};
use crate::settings::RecoverySettings;

use super::build_status_report;
use super::recovery::build_recovery_goal_report;

/// Project the open episode into the shape a `--json` client parses.
///
/// `now` is passed in rather than read from the clock so the whole surface
/// stays pure and `is_snoozed` is testable without waiting — the same reason
/// [`crate::cli::recovery`]'s builders take an [`FsUsage`] instead of calling
/// `statfs`.
pub fn build_pressure_episode_report(episode: &PressureEpisode, now: u64) -> PressureEpisodeReport {
    PressureEpisodeReport {
        episode_id: episode.episode_id,
        opened_unix_secs: episode.opened_unix_secs,
        opened_used_percent: episode.opened_used_percent,
        peak_used_percent: episode.peak_used_percent,
        latest_used_percent: episode.latest_used_percent,
        latest_free_bytes: episode.latest_free_bytes,
        latest_free_human: human_bytes(episode.latest_free_bytes),
        latest_unix_secs: episode.latest_unix_secs,
        notification_due: episode.notification_due,
        notifications_raised: episode.notifications_raised,
        last_notified_unix_secs: episode.last_notified_unix_secs,
        response: episode.response.map(|r| r.as_str()),
        responded_unix_secs: episode.responded_unix_secs,
        snoozed_until_unix_secs: episode.snoozed_until_unix_secs,
        is_snoozed: episode.is_snoozed_at(now),
    }
}

/// Build the whole pressure surface: the user's two settings, the volume as
/// measured now, and the episode (if any) that the app may need to raise.
///
/// Pure, for the reason above: `usage` is a reading the caller took and `now`
/// is the caller's clock.
///
/// `notification_due` is read straight off the tracker's episode and never
/// recomputed here. This module must not be a second place where "is a
/// notification owed?" is decided.
pub fn build_pressure_status_report(
    tracker: &EpisodeTracker,
    settings: &RecoverySettings,
    usage: &FsUsage,
    thresholds: &ThresholdConfig,
    now: u64,
    state_path: Option<String>,
) -> PressureStatusReport {
    let config = tracker.config();
    let episode = tracker
        .current()
        .map(|e| build_pressure_episode_report(e, now));
    PressureStatusReport {
        notify_at_used_percent: config.notify_at_used_percent(),
        notify_at_description: settings.describe_notify_at(),
        clear_at_used_percent: config.clear_at_used_percent(),
        snooze_secs: config.snooze().as_secs(),
        current: build_status_report(usage, thresholds),
        threshold_crossed: usage.used_percent() >= config.notify_at_used_percent(),
        notification_due: episode.as_ref().is_some_and(|e| e.notification_due),
        episode,
        default_goal: build_recovery_goal_report(&settings.default_goal()),
        responses: EpisodeResponse::ALL.iter().map(|r| r.as_str()).collect(),
        state_path,
    }
}

/// Project a refused answer or acknowledgement into the shape a `--json`
/// client parses.
pub fn build_pressure_rejection_report(rejection: &EpisodeRejection) -> PressureRejectionReport {
    PressureRejectionReport {
        reason: rejection.as_str(),
        message: rejection.to_string(),
    }
}

/// The prose `pressure show` prints, one line per element.
///
/// Built from the report rather than from the tracker, so the prose and the
/// JSON cannot describe two different states — the divergence would be
/// invisible in review and obvious only to a user comparing them.
///
/// Returned as lines rather than printed so the wording is testable without
/// capturing stdout, matching [`crate::cli::settings::describe_settings`] and
/// [`crate::autopilot::AutopilotEnvelope::describe`].
pub fn describe_pressure_status(report: &PressureStatusReport) -> Vec<String> {
    let mut lines = vec![
        format!(
            "disk usage:        {} ({} free of {})",
            used_percent_text(report.current.used_percent),
            report.current.free_human,
            report.current.total_human
        ),
        format!(
            "notify me at:      {} disk usage",
            report.notify_at_description
        ),
        // Named as a boundary on the *used* axis, like everything else here.
        // Without this line a user cannot tell why a disk at 74% used is still
        // in an episode when their threshold is 75% used.
        format!(
            "episode clears at: {}% used or below",
            configured_used_percent_figure(report.clear_at_used_percent)
        ),
        format!("recovery goal:     {}", report.default_goal.description),
    ];

    match &report.episode {
        None => {
            lines.push(
                "episode:           none — disk usage has not crossed the alert threshold"
                    .to_string(),
            );
            lines.push("notification:      not due".to_string());
        }
        Some(episode) => {
            lines.push(format!(
                "episode:           #{} since {}, peak {}",
                episode.episode_id,
                used_percent_text(episode.opened_used_percent),
                used_percent_text(episode.peak_used_percent)
            ));
            lines.push(format!(
                "notification:      {}",
                describe_notification_state(episode)
            ));
        }
    }

    lines.push(format!(
        "answers:           {}",
        report.responses.join(" | ")
    ));
    lines
}

/// One sentence covering every state a notification can be in, because
/// "raised" and "answered" and "snoozed" are four different situations for the
/// user and collapsing them into a bare boolean would hide the one thing they
/// might want to change.
fn describe_notification_state(episode: &PressureEpisodeReport) -> String {
    let raised = match episode.notifications_raised {
        0 => "not yet raised".to_string(),
        1 => "raised once".to_string(),
        n => format!("raised {n} times"),
    };
    let Some(tag) = episode.response else {
        return if episode.notification_due {
            format!("due — {raised}")
        } else {
            format!("{raised} — awaiting an answer")
        };
    };
    // Parsed back into the enum so the compiler enforces a line per answer: a
    // future fourth action must be worded here or the match stops compiling.
    // An unparseable tag is reported verbatim rather than folded into
    // "awaiting an answer", which would claim the user had not answered.
    match EpisodeResponse::parse(tag) {
        Some(EpisodeResponse::ReviewAndRecover) => {
            format!("{raised} — answered: review and recover")
        }
        Some(EpisodeResponse::RemindLater) => match episode.snoozed_until_unix_secs {
            Some(until) if episode.is_snoozed => {
                format!("{raised} — snoozed, reminder at unix {until}")
            }
            // The snooze deadline has passed but the daemon has not polled
            // since, so the reminder is owed and not yet due. Said plainly
            // rather than shown as "snoozed", which would be false.
            _ => format!("{raised} — snooze elapsed, reminder at the next poll"),
        },
        // Scoped to this episode and said so in the same breath: AC4 requires
        // that ignoring cannot read as disabling monitoring.
        Some(EpisodeResponse::IgnoreEpisode) => {
            format!("{raised} — ignored for this episode only; a new episode will notify again")
        }
        None => format!("{raised} — answered: {tag}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::episode::EpisodeConfig;

    const GIB: u64 = 1_073_741_824;

    fn settings(notify_at: f64, goal: f64) -> RecoverySettings {
        RecoverySettings::default()
            .with_changes(Some(notify_at), Some(goal))
            .expect("test pair must be valid")
    }

    /// A volume at `used_percent` of one TiB, so the reported usage and the
    /// episode's are the same figure.
    fn usage(used_percent: f64) -> FsUsage {
        let total = 1024 * GIB;
        let free = ((total as f64) * (1.0 - used_percent / 100.0)) as u64;
        FsUsage::new(total, free)
    }

    fn tracker_at(notify_at: f64) -> EpisodeTracker {
        EpisodeTracker::new(EpisodeConfig::new(notify_at))
    }

    fn report(tracker: &EpisodeTracker, used_percent: f64, now: u64) -> PressureStatusReport {
        build_pressure_status_report(
            tracker,
            &settings(75.0, 70.0),
            &usage(used_percent),
            &ThresholdConfig::default(),
            now,
            Some("/somewhere/pressure-episode.json".to_string()),
        )
    }

    #[test]
    fn a_healthy_disk_reports_no_episode_and_nothing_due() {
        let r = report(&tracker_at(75.0), 40.0, 1_000);
        assert!(r.episode.is_none());
        assert!(!r.notification_due);
        assert!(!r.threshold_crossed);
        assert_eq!(r.notify_at_used_percent, 75.0);
        assert_eq!(r.clear_at_used_percent, 72.0);
    }

    #[test]
    fn a_crossing_reports_the_episode_and_a_due_notification() {
        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        let r = report(&t, 94.0, 1_010);
        assert!(r.threshold_crossed);
        assert!(r.notification_due);
        let episode = r.episode.expect("an episode must be reported");
        assert_eq!(episode.episode_id, 1);
        assert!(episode.notification_due);
        assert_eq!(episode.notifications_raised, 0);
        assert_eq!(episode.opened_used_percent, 94.0);
        assert_eq!(episode.response, None);
        assert!(!episode.is_snoozed);
    }

    /// The field a client acts on must be false the moment the notification
    /// has been raised, or the app would re-raise it on its next poll. This is
    /// AC5 in one assertion.
    #[test]
    fn acknowledging_clears_the_only_field_a_client_acts_on() {
        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        assert!(report(&t, 94.0, 1_000).notification_due);
        assert!(t.mark_notified(1_010).is_ok());
        let r = report(&t, 94.0, 1_020);
        assert!(!r.notification_due);
        let episode = r.episode.unwrap();
        assert_eq!(episode.notifications_raised, 1);
        assert_eq!(episode.last_notified_unix_secs, Some(1_010));
        // Still an open episode: hysteresis has not cleared it, so the app can
        // show the pressure without being told to notify again.
        assert!(!episode.notification_due);
        assert_eq!(episode.latest_used_percent, 94.0);
    }

    /// Hysteresis is visible in the report: between the clear boundary and the
    /// threshold the episode is still open and `threshold_crossed` is false,
    /// and those two facts are reported separately rather than conflated.
    #[test]
    fn the_hysteresis_gap_is_reported_as_two_separate_facts() {
        let mut t = tracker_at(75.0);
        t.observe(76.0, 200 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        let r = report(&t, 73.5, 1_100);
        assert!(!r.threshold_crossed, "73.5% is below a 75% threshold");
        assert!(
            r.episode.is_some(),
            "but the episode is still open, which is what stops a hovering disk notifying twice"
        );
        assert!(!r.notification_due);
    }

    #[test]
    fn a_snooze_is_reported_as_a_deadline_and_a_derived_flag() {
        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::RemindLater, 1_010).unwrap();
        let snooze = t.config().snooze().as_secs();

        let during = report(&t, 94.0, 1_010 + snooze - 1);
        let episode = during.episode.clone().unwrap();
        assert_eq!(episode.snoozed_until_unix_secs, Some(1_010 + snooze));
        assert!(episode.is_snoozed);
        assert!(
            !during.notification_due,
            "a snooze must suppress the banner"
        );

        // Past the deadline the flag flips on the same stored state — no
        // second write is needed for a client to stop showing "snoozed".
        let after = report(&t, 94.0, 1_010 + snooze);
        assert!(!after.episode.unwrap().is_snoozed);
    }

    #[test]
    fn an_ignored_episode_reports_the_answer_and_stays_open() {
        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        t.mark_notified(1_000).unwrap();
        t.respond(EpisodeResponse::IgnoreEpisode, 1_010).unwrap();
        let r = report(&t, 94.0, 1_020);
        assert!(!r.notification_due);
        let episode = r.episode.unwrap();
        assert_eq!(episode.response, Some("ignore_episode"));
        assert_eq!(episode.responded_unix_secs, Some(1_010));
        assert_eq!(episode.snoozed_until_unix_secs, None);
    }

    /// The published answer set must be exactly what `pressure respond`
    /// accepts. Proven by parsing each published tag rather than by comparing
    /// the list to itself, so a tag that serialized but could not be sent back
    /// would fail here.
    #[test]
    fn every_published_answer_is_one_the_parser_accepts() {
        let r = report(&tracker_at(75.0), 40.0, 1_000);
        assert_eq!(r.responses.len(), EpisodeResponse::ALL.len());
        for tag in &r.responses {
            assert!(
                EpisodeResponse::parse(tag).is_some(),
                "published answer {tag} is not accepted by the parser"
            );
        }
        // And there is no "skip": HORO-1508 requires each action to say what
        // it does.
        assert!(
            !r.responses.iter().any(|t| t.contains("skip")),
            "{:?}",
            r.responses
        );
    }

    #[test]
    fn the_report_serializes_with_absent_optionals_omitted() {
        let json = serde_json::to_string(&report(&tracker_at(75.0), 40.0, 1_000)).unwrap();
        assert!(json.contains("\"notification_due\":false"), "{json}");
        assert!(json.contains("\"notify_at_used_percent\":75"), "{json}");
        assert!(!json.contains("\"episode\""), "{json}");

        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        let json = serde_json::to_string(&report(&t, 94.0, 1_000)).unwrap();
        assert!(json.contains("\"episode\""), "{json}");
        // Never answered, never notified, never snoozed: all three absent
        // rather than null.
        assert!(!json.contains("\"response\""), "{json}");
        assert!(!json.contains("last_notified_unix_secs"), "{json}");
        assert!(!json.contains("snoozed_until"), "{json}");
    }

    /// Four percentages can appear at once on this surface. None of them may
    /// be printed without saying which axis it is on.
    #[test]
    fn every_prose_line_with_a_percentage_names_its_axis() {
        let mut t = tracker_at(80.0);
        t.observe(92.5, 30 * GIB, 1_000);
        let lines = describe_pressure_status(&report(&t, 92.5, 1_000));
        for line in &lines {
            if line.contains('%') {
                assert!(line.contains("used"), "{line}");
            }
        }
        assert!(
            lines
                .iter()
                .any(|l| l.contains("episode clears at: 77% used or below")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("#1 since 92.5% used")),
            "{lines:#?}"
        );
    }

    /// HORO-1506. Every percentage on this surface is rendered by
    /// [`crate::reporting::used_percent`], and the two renderings it offers are
    /// not interchangeable: a measurement is truncated to a tenth, a figure the
    /// user configured is echoed back exactly.
    ///
    /// This surface is where the distinction earns its keep, because it is the
    /// one place all four percentages appear together. The local helper these
    /// lines used to share rendered every one of them with `{:.1}` — round to
    /// nearest — so an episode that opened at 89.96% used was printed as
    /// `#1 since 90% used`, two lines under a `notify me at: 90% used` that the
    /// daemon had decided was not crossed.
    #[test]
    fn a_measurement_is_truncated_and_a_configured_boundary_is_exact() {
        // A threshold with three decimals, and a reading just under it. The
        // shared `report` helper pins the settings at whole numbers, so this one
        // builds the report itself — the disagreement only appears when the
        // configured figure has digits to round.
        let mut t = tracker_at(87.456);
        t.observe(89.96, 30 * GIB, 1_000);
        let lines = describe_pressure_status(&build_pressure_status_report(
            &t,
            &settings(87.456, 70.0),
            &usage(89.96),
            &ThresholdConfig::default(),
            1_000,
            None,
        ));
        let line_with = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no line mentioning {needle} in {lines:#?}"))
                .clone()
        };

        // The measurements: truncated, so neither can read as having reached 90.
        assert!(
            line_with("disk usage:").contains("89.9% used"),
            "{lines:#?}"
        );
        let episode_line = line_with("episode:           #");
        assert!(
            episode_line.contains("#1 since 89.9% used"),
            "{episode_line}"
        );
        assert!(episode_line.contains("peak 89.9% used"), "{episode_line}");
        assert!(
            !episode_line.contains("90"),
            "the episode opened below the threshold and must not read as at it: {episode_line}"
        );

        // The configured figures: exact, so neither can read as above the
        // boundary its own comparison uses. `{:.2}` gave "87.46" for the
        // threshold and `{:.1}` gave "84.5" for the clear boundary; both sat
        // above the real number, which is the direction that misleads.
        let notify_line = line_with("notify me at:");
        assert!(notify_line.contains("87.456% used"), "{notify_line}");
        assert!(
            !notify_line.contains("87.46%"),
            "a threshold rounded up reads as one the daemon does not use: {notify_line}"
        );
        let clear_line = line_with("episode clears at:");
        assert!(clear_line.contains("84.456% used"), "{clear_line}");
        assert!(
            !clear_line.contains("84.5"),
            "a clear boundary rounded up claims an episode ends while it is still open: \
             {clear_line}"
        );
    }

    /// The four notification states read as four different sentences, so a
    /// user can tell "nobody has told me yet" from "you asked me to wait".
    #[test]
    fn each_notification_state_reads_differently() {
        let mut t = tracker_at(75.0);
        t.observe(94.0, 60 * GIB, 1_000);
        let due = notification_line(&report(&t, 94.0, 1_000));
        t.mark_notified(1_010).unwrap();
        let raised = notification_line(&report(&t, 94.0, 1_020));
        t.respond(EpisodeResponse::RemindLater, 1_020).unwrap();
        let snoozed = notification_line(&report(&t, 94.0, 1_030));
        t.respond(EpisodeResponse::IgnoreEpisode, 1_040).unwrap();
        let ignored = notification_line(&report(&t, 94.0, 1_050));

        assert!(due.contains("due"), "{due}");
        assert!(due.contains("not yet raised"), "{due}");
        assert!(raised.contains("awaiting an answer"), "{raised}");
        assert!(snoozed.contains("snoozed"), "{snoozed}");
        // Scoped, and the prose says so — AC4 in the wording, not just in the
        // state machine.
        assert!(ignored.contains("this episode only"), "{ignored}");
        assert!(
            ignored.contains("a new episode will notify again"),
            "{ignored}"
        );

        let all = [&due, &raised, &snoozed, &ignored];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two notification states read identically");
            }
        }
    }

    fn notification_line(report: &PressureStatusReport) -> String {
        describe_pressure_status(report)
            .into_iter()
            .find(|l| l.starts_with("notification:"))
            .expect("a notification line must always be printed")
    }

    /// A healthy disk still prints a notification line, so the absence of an
    /// episode cannot be mistaken for the absence of monitoring.
    #[test]
    fn a_healthy_disk_still_prints_every_line() {
        let lines = describe_pressure_status(&report(&tracker_at(75.0), 40.0, 1_000));
        assert!(
            lines.iter().any(|l| l.starts_with("disk usage:")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("notify me at:")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("episode clears at:")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("recovery goal:")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l == "notification:      not due"),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("answers:")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_refusal_reports_the_domain_types_own_tag_and_wording() {
        let rejection = EpisodeRejection::NoOpenEpisode;
        let report = build_pressure_rejection_report(&rejection);
        assert_eq!(report.reason, "no_open_episode");
        assert_eq!(report.message, rejection.to_string());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"reason\":\"no_open_episode\""), "{json}");
    }

    /// The prose and the JSON are built from one report, so they cannot
    /// disagree about whether a notification is owed. Proven by reading the
    /// state out of the prose and comparing it to the field.
    #[test]
    fn the_prose_and_the_field_never_disagree() {
        let mut t = tracker_at(75.0);
        let cases: Vec<(f64, u64)> =
            vec![(40.0, 1_000), (94.0, 1_100), (94.0, 1_200), (50.0, 1_300)];
        for (used, now) in cases {
            t.observe(used, 10 * GIB, now);
            let r = report(&t, used, now);
            let line = notification_line(&r);
            assert_eq!(
                line.contains("due — "),
                r.notification_due,
                "prose {line:?} disagrees with notification_due={}",
                r.notification_due
            );
            if r.notification_due {
                t.mark_notified(now).unwrap();
            }
        }
    }
}
