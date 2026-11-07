//! The before/after receipt of one `free` run (HORO-2362).
//!
//! After a manual cleanup the only trustworthy number was `df` before and
//! after; the sum of the directories' own sizes disagreed with it by several
//! GiB and nothing recorded which kind or root had taken the space. The
//! receipt keeps the two kinds of figure apart:
//!
//! - **measured**: the volume's free bytes before and after, and their
//!   difference. This is the only figure that says what the disk gained.
//! - **estimated**: the size estimates of the actions that ran, grouped by
//!   resource kind and by root. Estimates can overlap and can be lower bounds.
//!
//! The two are never added, subtracted or compared into one number, and
//! nothing here says one "accounts for" the other (ADR-0001 §3.7, §19).
//!
//! # Report-only
//!
//! The receipt and its growth entries are built after the run has finished,
//! from data the run recorded and never read. They reach no decision: the
//! deciding layers do not name this module, which
//! `tests/run_receipt_has_no_authority.rs` enforces. Age ("idle N days") is
//! explanation only; Config's rule is that age alone is never a reason.
//!
//! Pure on purpose. Reading and writing the sample history is the caller's job,
//! so a history that cannot be read arrives here as
//! [`HistoryState::Unavailable`] and costs the growth figures, not the receipt.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::executor::recovery_loop::{ExecutedStep, ObservedCandidate, RecoveryReport};
use crate::monitor::resource_history::{HistoryState, Sample};
use crate::reporting::human_bytes;
use crate::workspace::history::LocalAlias;

/// Groups shown per axis. Beyond it the receipt says so rather than folding
/// the rest into an "other" bucket, which would be a sum nobody measured.
pub const MAX_GROUPS: usize = 20;

/// Growth entries shown, largest first.
pub const MAX_GROWTH_ENTRIES: usize = 20;

/// Candidates sampled per run into the history.
pub const MAX_RECORDED_SAMPLES: usize = 64;

const SECS_PER_DAY: u64 = 24 * 60 * 60;

const SEPARATION_NOTE: &str =
    "Measured and estimated figures are reported separately and are not added or compared.";

/// The wire shape of the receipt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunReceipt {
    /// Absent when a reading of the volume failed during the run, so there is no
    /// trustworthy before and after to report (`notes` says so).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<ReceiptMeasured>,
    pub estimated: ReceiptEstimated,
    /// Explanation-only. Empty when there is no history to compare against.
    pub growth: Vec<ResourceGrowth>,
    /// `"available"` or `"unavailable"`. `"available"` with no earlier sample
    /// for a resource means first sighting, not "did not grow".
    pub history: &'static str,
    pub notes: Vec<String>,
}

/// What the volume did, from two `statvfs` readings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceiptMeasured {
    pub free_bytes_before: u64,
    pub free_bytes_after: u64,
    /// `after - before`; negative when free space shrank during the run.
    pub volume_free_delta: i64,
    pub volume_free_delta_human: String,
}

/// What the executed actions were expected to free, by two groupings of the
/// same actions. Each axis is a partition of the same set, so the axes must
/// not be added to each other either.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceiptEstimated {
    pub by_kind: Vec<ReceiptGroup>,
    pub by_root: Vec<ReceiptGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceiptGroup {
    /// A resource kind tag, or a root path.
    pub key: String,
    /// Succeeded actions in the group.
    pub actions: u32,
    /// Sum of the estimates that existed. Actions without one add nothing here
    /// and are counted in `unestimated_actions`.
    pub estimated_bytes: u64,
    pub estimated_human: String,
    pub unestimated_actions: u32,
}

/// What the stored samples say about one candidate the run saw.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceGrowth {
    pub kind: String,
    pub root: String,
    pub estimated_bytes: u64,
    /// Change since the previous sample. `null` without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grew_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_sampled_unix_secs: Option<u64>,
    /// Whole days since the resource's newest observed modification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u64>,
    pub summary: String,
}

/// The opaque key a resource is stored under.
pub fn resource_key(resource: &str) -> u64 {
    LocalAlias::of_name(resource).as_u64()
}

/// Builds the receipt for a finished run.
pub fn build_run_receipt(
    report: &RecoveryReport,
    now_unix_secs: u64,
    history: &HistoryState,
) -> RunReceipt {
    let mut notes = vec![SEPARATION_NOTE.to_string()];

    let succeeded: Vec<&ExecutedStep> = report
        .executed_steps
        .iter()
        .filter(|step| step.succeeded)
        .collect();
    let (by_kind, kind_total) = group(&succeeded, |step| step.kind.to_string());
    let (by_root, root_total) = group(&succeeded, |step| step.root.clone());
    if kind_total > MAX_GROUPS || root_total > MAX_GROUPS {
        notes.push(format!(
            "Only the {MAX_GROUPS} largest groups per axis are listed."
        ));
    }

    let growth = match history {
        HistoryState::Available(history) => {
            let mut entries: Vec<&ObservedCandidate> = report
                .observed_candidates
                .iter()
                .filter(|c| c.estimated_bytes.is_some())
                .collect();
            entries.sort_by(|a, b| {
                b.estimated_bytes
                    .cmp(&a.estimated_bytes)
                    .then_with(|| a.resource.cmp(&b.resource))
            });
            entries
                .into_iter()
                .take(MAX_GROWTH_ENTRIES)
                .map(|candidate| {
                    let previous =
                        history.latest_before(resource_key(&candidate.resource), now_unix_secs);
                    growth_entry(candidate, previous, now_unix_secs)
                })
                .collect()
        }
        HistoryState::Unavailable => {
            notes.push("Size history could not be read, so no growth is reported.".to_string());
            Vec::new()
        }
    };

    let measured = if report.volume_readings_complete {
        let delta = i128::from(report.final_free_bytes) - i128::from(report.started_free_bytes);
        let volume_free_delta =
            i64::try_from(delta).unwrap_or(if delta < 0 { i64::MIN } else { i64::MAX });
        let sign = if delta < 0 { "-" } else { "" };
        Some(ReceiptMeasured {
            free_bytes_before: report.started_free_bytes,
            free_bytes_after: report.final_free_bytes,
            volume_free_delta,
            volume_free_delta_human: format!(
                "{sign}{}",
                human_bytes(u64::try_from(delta.unsigned_abs()).unwrap_or(u64::MAX))
            ),
        })
    } else {
        notes.push(
            "The volume's free space could not be read before or after the run, so no measured change is reported."
                .to_string(),
        );
        None
    };

    RunReceipt {
        measured,
        estimated: ReceiptEstimated { by_kind, by_root },
        growth,
        history: match history {
            HistoryState::Available(_) => "available",
            HistoryState::Unavailable => "unavailable",
        },
        notes,
    }
}

/// The samples to store for this run: the largest candidates that had an
/// estimate.
pub fn samples_to_record(report: &RecoveryReport, now_unix_secs: u64) -> Vec<(u64, Sample)> {
    let mut sized: Vec<(&ObservedCandidate, u64)> = report
        .observed_candidates
        .iter()
        .filter_map(|c| c.estimated_bytes.map(|bytes| (c, bytes)))
        .collect();
    sized.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.resource.cmp(&b.0.resource)));
    sized
        .into_iter()
        .take(MAX_RECORDED_SAMPLES)
        .map(|(candidate, bytes)| {
            (
                resource_key(&candidate.resource),
                Sample {
                    at_unix_secs: now_unix_secs,
                    bytes,
                    last_modified_unix_secs: candidate.last_modified_unix_secs,
                },
            )
        })
        .collect()
}

/// Groups steps by `key_of`, largest first, and returns the group count before
/// truncation.
fn group(
    steps: &[&ExecutedStep],
    key_of: impl Fn(&ExecutedStep) -> String,
) -> (Vec<ReceiptGroup>, usize) {
    let mut groups: BTreeMap<String, (u32, u64, u32)> = BTreeMap::new();
    for step in steps {
        let entry = groups.entry(key_of(step)).or_default();
        entry.0 += 1;
        match step.estimated_bytes {
            Some(bytes) => entry.1 = entry.1.saturating_add(bytes),
            None => entry.2 += 1,
        }
    }
    let total = groups.len();
    let mut listed: Vec<ReceiptGroup> = groups
        .into_iter()
        .map(|(key, (actions, bytes, unestimated))| ReceiptGroup {
            key,
            actions,
            estimated_bytes: bytes,
            estimated_human: human_bytes(bytes),
            unestimated_actions: unestimated,
        })
        .collect();
    listed.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| a.key.cmp(&b.key))
    });
    listed.truncate(MAX_GROUPS);
    (listed, total)
}

fn growth_entry(
    candidate: &ObservedCandidate,
    previous: Option<&Sample>,
    now_unix_secs: u64,
) -> ResourceGrowth {
    let bytes = candidate.estimated_bytes.unwrap_or(0);
    let grew_bytes = previous.map(|p| {
        let delta = i128::from(bytes) - i128::from(p.bytes);
        i64::try_from(delta).unwrap_or(if delta < 0 { i64::MIN } else { i64::MAX })
    });
    let idle_days = candidate
        .last_modified_unix_secs
        .map(|mtime| now_unix_secs.saturating_sub(mtime) / SECS_PER_DAY);

    let mut parts = Vec::new();
    match grew_bytes {
        Some(0) => parts.push("unchanged since last run".to_string()),
        Some(delta) if delta > 0 => parts.push(format!(
            "grew {} since last run",
            human_bytes(delta.unsigned_abs())
        )),
        Some(delta) => parts.push(format!(
            "shrank {} since last run",
            human_bytes(delta.unsigned_abs())
        )),
        None => parts.push("no earlier sample".to_string()),
    }
    if let Some(days) = idle_days.filter(|days| *days >= 1) {
        parts.push(format!("idle {days} days"));
    }

    ResourceGrowth {
        kind: candidate.kind.to_string(),
        root: candidate.root.clone(),
        estimated_bytes: bytes,
        grew_bytes,
        previous_sampled_unix_secs: previous.map(|p| p.at_unix_secs),
        idle_days,
        summary: parts.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::recovery_loop::StopReason;
    use crate::monitor::resource_history::ResourceHistory;

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = SECS_PER_DAY;

    fn step(kind: &'static str, root: &str, est: Option<u64>, ok: bool) -> ExecutedStep {
        ExecutedStep {
            resource: format!("{kind}:{root}/x"),
            kind,
            root: root.to_string(),
            estimated_bytes: est,
            measured_reclaimed_bytes: None,
            succeeded: ok,
        }
    }

    fn report(
        steps: Vec<ExecutedStep>,
        observed: Vec<ObservedCandidate>,
        before: u64,
        after: u64,
    ) -> RecoveryReport {
        RecoveryReport {
            stop_reason: StopReason::TargetReached,
            iterations_run: steps.len() as u32,
            actions_executed: steps.len() as u32,
            actions_declined_or_skipped: 0,
            total_bytes_freed: 0,
            started_free_bytes: before,
            final_free_bytes: after,
            detector_failures: vec![],
            detectors_not_examined: vec![],
            host_integrity_notes: vec![],
            executed_steps: steps,
            observed_candidates: observed,
            volume_readings_complete: true,
        }
    }

    fn candidate(resource: &str, bytes: Option<u64>, mtime: Option<u64>) -> ObservedCandidate {
        ObservedCandidate {
            resource: resource.to_string(),
            kind: "cargo_target_dir",
            root: "/p".to_string(),
            estimated_bytes: bytes,
            last_modified_unix_secs: mtime,
        }
    }

    fn available(entries: &[(u64, Sample)]) -> HistoryState {
        HistoryState::Available(ResourceHistory::from_samples(NOW, entries))
    }

    #[test]
    fn measured_delta_is_after_minus_before_and_can_be_negative() {
        let up = build_run_receipt(
            &report(vec![], vec![], 100, 350),
            NOW,
            &HistoryState::Unavailable,
        );
        assert_eq!(up.measured.as_ref().unwrap().volume_free_delta, 250);
        let down = build_run_receipt(
            &report(vec![], vec![], 350, 100),
            NOW,
            &HistoryState::Unavailable,
        );
        assert_eq!(down.measured.as_ref().unwrap().volume_free_delta, -250);
        assert_eq!(
            down.measured.as_ref().unwrap().volume_free_delta_human,
            "-250 B"
        );
    }

    #[test]
    fn a_failed_volume_reading_gives_no_measured_figure() {
        let mut incomplete = report(vec![], vec![], 0, 0);
        incomplete.volume_readings_complete = false;
        let r = build_run_receipt(&incomplete, NOW, &HistoryState::Unavailable);
        assert!(r.measured.is_none());
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("could not be read before or after")));
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("measured").is_none());
    }

    #[test]
    fn estimated_groups_never_include_the_measured_delta_or_failed_actions() {
        let steps = vec![
            step("cargo_target_dir", "/a", Some(1000), true),
            step("cargo_target_dir", "/b", Some(500), true),
            step("node_modules", "/a", None, true),
            step("node_modules", "/a", Some(9_999), false),
        ];
        let r = build_run_receipt(
            &report(steps, vec![], 0, 7),
            NOW,
            &HistoryState::Available(ResourceHistory::default()),
        );
        let kinds: Vec<_> = r
            .estimated
            .by_kind
            .iter()
            .map(|g| {
                (
                    g.key.as_str(),
                    g.actions,
                    g.estimated_bytes,
                    g.unestimated_actions,
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![("cargo_target_dir", 2, 1500, 0), ("node_modules", 1, 0, 1)]
        );
        let roots: Vec<_> = r
            .estimated
            .by_root
            .iter()
            .map(|g| (g.key.as_str(), g.estimated_bytes))
            .collect();
        assert_eq!(roots, vec![("/a", 1000), ("/b", 500)]);
        // 7 measured is nowhere in the estimated figures, and vice versa.
        assert_eq!(r.measured.as_ref().unwrap().volume_free_delta, 7);
    }

    #[test]
    fn unavailable_history_keeps_the_receipt_and_drops_growth() {
        let steps = vec![step("cargo_target_dir", "/a", Some(10), true)];
        let obs = vec![candidate(
            "cargo_target_dir:/p/target",
            Some(10),
            Some(NOW - 5 * DAY),
        )];
        let r = build_run_receipt(&report(steps, obs, 1, 2), NOW, &HistoryState::Unavailable);
        assert_eq!(r.history, "unavailable");
        assert!(r.growth.is_empty());
        assert_eq!(r.estimated.by_kind.len(), 1);
        assert!(r.notes.iter().any(|n| n.contains("could not be read")));
    }

    #[test]
    fn growth_compares_against_the_previous_sample_and_reports_idle_days() {
        let key = resource_key("cargo_target_dir:/p/target");
        let prior = Sample {
            at_unix_secs: NOW - 2 * DAY,
            bytes: 1_000,
            last_modified_unix_secs: None,
        };
        let history = available(&[(key, prior)]);
        let obs = vec![
            candidate(
                "cargo_target_dir:/p/target",
                Some(3_000),
                Some(NOW - 66 * DAY),
            ),
            candidate("cargo_target_dir:/p/new", Some(10), Some(NOW - 100)),
        ];
        let r = build_run_receipt(&report(vec![], obs, 0, 0), NOW, &history);
        assert_eq!(r.growth[0].grew_bytes, Some(2_000));
        assert_eq!(r.growth[0].idle_days, Some(66));
        assert_eq!(
            r.growth[0].summary,
            "grew 2.0 KB since last run, idle 66 days"
        );
        assert_eq!(r.growth[1].grew_bytes, None);
        assert_eq!(r.growth[1].summary, "no earlier sample");
    }

    #[test]
    fn samples_to_record_skips_candidates_without_an_estimate() {
        let obs = vec![
            candidate("a", Some(5), Some(1)),
            candidate("b", None, Some(1)),
        ];
        let samples = samples_to_record(&report(vec![], obs, 0, 0), NOW);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].0, resource_key("a"));
        assert_eq!(samples[0].1.at_unix_secs, NOW);
    }
}
