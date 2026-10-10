//! Pre/post host-dependency integrity comparison (ADR-0001 §4.4,
//! HORO-1826).
//!
//! This module deliberately contains NO new probing logic. It is a pure
//! comparison over two [`ExecutableDependencyReport`]s that were each
//! produced by re-running the SAME HORO-1825 probe
//! (`HostDependencyProbe::probe`) — once before an action mutates
//! anything, once after. Glomeris reports and refuses; it never repairs,
//! reinstalls, or restarts anything here (carried forward unchanged from
//! HORO-1825/1823/1824).
//!
//! The central fail-closed rule, stated once: a probe FAILURE on either
//! side must never read as "nothing changed, still OK". That exact shape
//! sank the first review round of HORO-1823, HORO-1824 and HORO-1825 —
//! see [`compare_host_integrity`]'s doc comment for how this module closes
//! it structurally rather than by convention.

use crate::evidence::model::{
    DependencyIntegrityEntry, ExecutableDependencyReport, HostIntegrityReport, IntegrityStatus,
};
use crate::evidence::probe::ProbeOutcome;

/// Combine two statuses, keeping whichever withholds the most confidence.
/// `Degraded` beats `Unverified` beats `Ok`. There is no `Default` and no
/// identity element that is `Ok` — folding an empty sequence must start
/// from `Ok` explicitly at the call site, never implicitly.
fn combine(a: IntegrityStatus, b: IntegrityStatus) -> IntegrityStatus {
    use IntegrityStatus::*;
    match (a, b) {
        (Degraded, _) | (_, Degraded) => Degraded,
        (Unverified, _) | (_, Unverified) => Unverified,
        (Ok, Ok) => Ok,
    }
}

/// Compare a pre-action and a post-action [`ExecutableDependencyReport`]
/// probe outcome (HORO-1826 §4.4).
///
/// `post` uses the SAME [`ProbeOutcome`] shape the pre-action probe does,
/// deliberately: there is no separate "the run crashed before the
/// post-probe" variant, because that case and "the post-probe ran and
/// failed" demand the identical fail-closed answer (`Unverified`) — ADR-
/// 0001 §4.4 states them as one bullet ("either probe failed, or the run
/// crashed before the post-probe"). A caller whose action aborted before
/// ever attempting the post-probe passes
/// `ProbeOutcome::Unavailable(ProbeReason::NotAttempted)`, which this
/// function treats exactly like a genuine probe failure — never as an
/// absent field that silently defaults to OK.
///
/// Adversarial note (the HORO-1823/1824/1825 recurring review finding,
/// closed here structurally): every branch below that cannot positively
/// confirm "same resolved path, same (dev, ino)" produces `Degraded` or
/// `Unverified`, never falls through to an implicit `Ok`. In particular:
/// - `pre` itself `Unavailable` => `Unverified` immediately, zero entries
///   (there is nothing to compare against).
/// - `post` `Unavailable` while `pre` found a reference => that
///   reference is `Unverified`, not "unchanged".
/// - A pre-action reference absent from `post`'s `references_inside`, and
///   not explained by a matching `unresolved` entry on the same source =>
///   `Degraded` (genuinely gone, not merely unresolved).
/// - A pre-action reference absent from `post`'s `references_inside`, but
///   the same source IS present in `post.unresolved` => `Unverified`
///   (post-probe could not confirm either way for that source).
/// - Either side having any `unresolved` entries at all widens the
///   run-level status to at least `Unverified`, even if every individual
///   dependency compared clean — an incomplete sweep is not evidence of a
///   clean one.
///
/// Only file identity (`resolved` + `exe`) ever drives `Degraded`. A
/// process that was running pre-action and has exited by the time the
/// post-probe runs is read correctly as benign here, because this
/// function never inspects `running_inside` at all — see
/// [`ExecutableDependencyReport::running_inside`]'s doc comment and the
/// module docs above.
pub fn compare_host_integrity(
    pre: &ProbeOutcome<ExecutableDependencyReport>,
    post: &ProbeOutcome<ExecutableDependencyReport>,
) -> HostIntegrityReport {
    let Some(pre_report) = pre.observed() else {
        // The pre-action probe itself never produced a report. There is
        // nothing to compare against, so nothing can be claimed clean.
        return HostIntegrityReport {
            status: IntegrityStatus::Unverified,
            entries: Vec::new(),
        };
    };

    let post_report = post.observed();
    let mut status = IntegrityStatus::Ok;

    // An incomplete pre-action sweep means some reference may have gone
    // unexamined before anything even ran — the baseline itself is not
    // trustworthy.
    if !pre_report.unresolved.is_empty() {
        status = combine(status, IntegrityStatus::Unverified);
    }

    match post_report {
        None => {
            // The post-probe failed, or the run crashed/aborted before it
            // could run at all (see this function's doc comment for why
            // those two are not distinguished). If there was anything to
            // verify, or the baseline was already incomplete, this run is
            // Unverified as a whole — never silently OK because nothing
            // new was observed.
            if !pre_report.references_inside.is_empty() || !pre_report.unresolved.is_empty() {
                status = combine(status, IntegrityStatus::Unverified);
            }
        }
        Some(post_report) => {
            if !post_report.unresolved.is_empty() {
                status = combine(status, IntegrityStatus::Unverified);
            }
        }
    }

    let mut entries = Vec::with_capacity(pre_report.references_inside.len());
    for dep in &pre_report.references_inside {
        let dep_status = match post_report {
            None => IntegrityStatus::Unverified,
            Some(post_report) => match post_report
                .references_inside
                .iter()
                .find(|post_dep| post_dep.source == dep.source && post_dep.pointer == dep.pointer)
            {
                Some(post_dep) if post_dep.resolved == dep.resolved && post_dep.exe == dep.exe => {
                    IntegrityStatus::Ok
                }
                // Same structural pointer, but the resolved path or the
                // (dev, ino) identity differs — genuinely changed.
                Some(_) => IntegrityStatus::Degraded,
                None => {
                    // Not found in the post-action references. If the
                    // post-probe itself flagged this exact source as
                    // unresolved, the absence does not confirm removal —
                    // fail closed to Unverified rather than reading a gap
                    // as a clean negative. Otherwise, the post-probe
                    // examined this source cleanly and found nothing
                    // there: genuinely missing.
                    if post_report
                        .unresolved
                        .iter()
                        .any(|u| u.source() == &dep.source)
                    {
                        IntegrityStatus::Unverified
                    } else {
                        IntegrityStatus::Degraded
                    }
                }
            },
        };
        status = combine(status, dep_status);
        entries.push(DependencyIntegrityEntry {
            source: dep.source.clone(),
            pointer: dep.pointer.clone(),
            pre_resolved: dep.resolved.clone(),
            pre_exe: dep.exe.clone(),
            status: dep_status,
        });
    }

    HostIntegrityReport { status, entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::model::{
        DependencyRef, ExeIdentity, Provenance, SourceTag, UnresolvedRef,
    };
    use crate::evidence::probe::ProbeReason;
    use std::path::PathBuf;

    fn exe(ino: u64) -> ExeIdentity {
        ExeIdentity {
            path: PathBuf::from(format!("/resource/bin/hook-{ino}")),
            dev: 1,
            ino,
        }
    }

    fn dep_ref(ino: u64) -> DependencyRef {
        DependencyRef {
            source: SourceTag::ClaudeUserSettings,
            pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            resolved: PathBuf::from(format!("/resource/bin/hook-{ino}")),
            exe: exe(ino),
            provenance: Provenance::Observed,
            next_invocation: true,
        }
    }

    fn clean(refs: Vec<DependencyRef>) -> ProbeOutcome<ExecutableDependencyReport> {
        ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: refs,
            running_inside: Vec::new(),
            unresolved: Vec::new(),
            sources_examined: vec![SourceTag::ClaudeUserSettings, SourceTag::PathEntry],
        })
    }

    /// AC negative control: a disposable hook-dependency target — present
    /// pre-action, missing post-action — must record DEGRADED, never OK.
    #[test]
    fn missing_post_action_dependency_is_degraded() {
        let pre = clean(vec![dep_ref(1)]);
        let post = clean(vec![]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Degraded);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, IntegrityStatus::Degraded);
    }

    /// AC: a valid, unchanged dependency (same path, same dev/ino both
    /// sides) is OK.
    #[test]
    fn unchanged_dependency_is_ok() {
        let pre = clean(vec![dep_ref(1)]);
        let post = clean(vec![dep_ref(1)]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Ok);
        assert_eq!(report.entries[0].status, IntegrityStatus::Ok);
    }

    /// A dependency whose (dev, ino) changed without disappearing (e.g.
    /// replaced in place) is DEGRADED, not OK — identity, not mere
    /// presence, is what's compared.
    #[test]
    fn identity_changed_dependency_is_degraded() {
        let pre = clean(vec![dep_ref(1)]);
        let post = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: vec![DependencyRef {
                exe: exe(2),
                ..dep_ref(1)
            }],
            running_inside: Vec::new(),
            unresolved: Vec::new(),
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Degraded);
    }

    /// AC: an unknown/failed post-action probe must NEVER show all-clear —
    /// it must be UNVERIFIED, even though a naive implementation could
    /// read "post probe unavailable, nothing changed" as OK.
    #[test]
    fn failed_post_probe_is_unverified_not_ok() {
        let pre = clean(vec![dep_ref(1)]);
        let post = ProbeOutcome::Unavailable(ProbeReason::Failed);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Unverified);
        assert_eq!(report.entries[0].status, IntegrityStatus::Unverified);
    }

    /// AC: a failed PRE-action probe must also never default to OK.
    #[test]
    fn failed_pre_probe_is_unverified() {
        let pre = ProbeOutcome::Unavailable(ProbeReason::TimedOut);
        let post = clean(vec![]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Unverified);
        assert!(report.entries.is_empty());
    }

    /// AC: "crash before health probe" — the run aborted before the
    /// post-probe ever attempted. Modelled as `Unavailable(NotAttempted)`
    /// per this function's doc comment. Must show UNVERIFIED, never
    /// silently omitted or defaulted to OK.
    #[test]
    fn crash_before_post_probe_is_unverified() {
        let pre = clean(vec![dep_ref(1)]);
        let post = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Unverified);
        assert_eq!(report.entries[0].status, IntegrityStatus::Unverified);
    }

    /// A pre-action reference missing from post's `references_inside`,
    /// but explained by a matching `unresolved` entry on the same source
    /// (the post-probe could not resolve that source at all) is
    /// UNVERIFIED, not a confirmed DEGRADED — the gap is in the probe,
    /// not necessarily in the dependency.
    #[test]
    fn missing_dependency_explained_by_post_unresolved_is_unverified_not_degraded() {
        let pre = clean(vec![dep_ref(1)]);
        let post = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: Vec::new(),
            unresolved: vec![UnresolvedRef::SourceUnreadable {
                source: SourceTag::ClaudeUserSettings,
            }],
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Unverified);
        assert_eq!(report.entries[0].status, IntegrityStatus::Unverified);
    }

    /// Process-exit-after-deletion: a process that was running pre-action
    /// has exited by the time the post-probe runs, but the dependency
    /// FILE itself is unchanged. This must read as benign (OK), not
    /// DEGRADED — only file identity drives degradation, never process
    /// liveness. Modelled by a `running_inside` entry present pre-action
    /// and absent post-action, with the file-based `references_inside`
    /// entry identical both times.
    #[test]
    fn process_exit_with_unchanged_file_dependency_is_ok() {
        use crate::evidence::model::ProcessRef;

        let pre = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: vec![dep_ref(1)],
            running_inside: vec![ProcessRef::new(4242, "hook-1".to_string())],
            unresolved: Vec::new(),
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });
        // The process has exited by post-probe time: `running_inside` is
        // now empty. The file reference is untouched.
        let post = clean(vec![dep_ref(1)]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Ok);
        assert_eq!(report.entries[0].status, IntegrityStatus::Ok);
    }

    /// Mutation test (ADR-0001 §13 anti-vacuity shape): a run-level status
    /// computation that ignores one DEGRADED dependency must go RED. This
    /// test asserts the real, non-mutated behavior — restoring `combine`
    /// to ignore a `Degraded` entry (e.g. always returning `Ok` once any
    /// entry exists) would fail this assertion.
    #[test]
    fn one_degraded_dependency_among_several_still_degrades_the_whole_run() {
        let pre = clean(vec![dep_ref(1), dep_ref(2)]);
        // dep_ref(1) survives unchanged; dep_ref(2) is gone.
        let post = clean(vec![dep_ref(1)]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Degraded);
        assert_eq!(report.entries.len(), 2);
        assert!(report
            .entries
            .iter()
            .any(|e| e.status == IntegrityStatus::Degraded));
        assert!(report
            .entries
            .iter()
            .any(|e| e.status == IntegrityStatus::Ok));
    }

    /// Mutation test: an incomplete source sweep (non-empty `unresolved`)
    /// on either side must widen the run-level status away from `Ok`,
    /// even when every individually-matched dependency came back clean.
    #[test]
    fn incomplete_sources_examined_prevents_a_clean_ok_even_with_matching_dependencies() {
        let pre = clean(vec![dep_ref(1)]);
        let post = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: vec![dep_ref(1)],
            running_inside: Vec::new(),
            // Unrelated source failed to resolve — the sweep as a whole
            // was not complete, even though dep_ref(1) matched cleanly.
            unresolved: vec![UnresolvedRef::SourceUnreadable {
                source: SourceTag::CodexHooks,
            }],
            sources_examined: vec![SourceTag::ClaudeUserSettings, SourceTag::CodexHooks],
        });

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Unverified);
        // The matched dependency itself is still reported OK individually
        // — the incompleteness is a run-level fact, not a per-dependency
        // one.
        assert_eq!(report.entries[0].status, IntegrityStatus::Ok);
    }

    /// No dependencies to protect on either side: vacuously OK, and the
    /// baseline/post sweep were both complete.
    #[test]
    fn no_dependencies_either_side_is_ok() {
        let pre = clean(vec![]);
        let post = clean(vec![]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Ok);
        assert!(report.entries.is_empty());
    }
}
