//! Pre/post host-dependency integrity comparison (ADR-0001 §4.4,
//! HORO-1826).
//!
//! [`compare_host_integrity`] deliberately contains NO new probing logic:
//! it is a pure comparison over two [`ExecutableDependencyReport`]s.
//! Glomeris reports and refuses; it never repairs, reinstalls, or
//! restarts anything here (carried forward unchanged from
//! HORO-1825/1823/1824).
//!
//! The central fail-closed rule, stated once: a probe FAILURE on either
//! side must never read as "nothing changed, still OK". That exact shape
//! sank the first review round of HORO-1823, HORO-1824 and HORO-1825 —
//! see [`compare_host_integrity`]'s doc comment for how this module closes
//! it structurally rather than by convention.
//!
//! [`restat_post_action`] builds the POST-action report that
//! [`compare_host_integrity`] compares against — deliberately NOT by
//! re-running [`super::host_dependency::HostDependencyProbe::probe`]
//! again. That probe's first step is `fs::canonicalize(resource_path)`,
//! which fails the instant a successful cleanup has deleted the
//! resource — exactly the case this ticket exists to verify. Re-running
//! it post-action would report every real cleanup as "probe failed"
//! regardless of whether the tracked dependency itself survived, which
//! is vacuous, not safe (a prior draft of this module had exactly that
//! bug — see HORO-1826's PR discussion). Instead, each already-known
//! dependency's own resolved path is re-stat'd directly via
//! [`DependencyRestat`] — no symlink-chain walk, no host-config re-read,
//! no subprocess, and well-defined whether or not the resource itself
//! still exists.

use std::path::Path;

use crate::evidence::model::{
    DependencyIntegrityEntry, DependencyRef, ExecutableDependencyReport, HostIntegrityReport,
    IntegrityStatus, UnresolvedRef,
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
            Some(post_report) => match post_report.references_inside.iter().find(|post_dep| {
                // Matches on `(source, pointer, resolved)`, not
                // `(source, pointer)` (round-3 independent review of PR
                // #163): `resolve_command` can legitimately push multiple
                // `DependencyRef`s sharing one exact `(source, pointer)`
                // (e.g. a command whose head and an absolute-path argument
                // both resolve inside the resource), each with its own
                // `resolved` path. Matching on `(source, pointer)` alone
                // would let `.find()` return the WRONG sibling's post-
                // action entry, mis-pairing one dependency's survival
                // against another's deletion — always in the safe
                // direction (a mis-pair can only ever read a genuinely-OK
                // dependency as `Degraded`, never the reverse, since a
                // mismatched `exe`/`resolved` falls through to the
                // `Degraded` arm below), but still the same root cause as
                // the `unresolved`-matching fix just below it, so fixed in
                // the same pass.
                post_dep.source == dep.source
                    && post_dep.pointer == dep.pointer
                    && post_dep.resolved == dep.resolved
            }) {
                Some(post_dep) if post_dep.exe == dep.exe => IntegrityStatus::Ok,
                // Same `(source, pointer, resolved)`, but the (dev, ino)
                // identity differs — genuinely changed in place.
                Some(_) => IntegrityStatus::Degraded,
                None => {
                    // Not found in the post-action references. If the
                    // post-probe itself flagged THIS dependency's own
                    // source/pointer/resolved as unresolved, the absence
                    // does not confirm removal — fail closed to Unverified
                    // rather than reading a gap as a clean negative.
                    // Otherwise, the post-probe examined this source
                    // cleanly and found nothing there: genuinely missing.
                    //
                    // Matches on `(source, pointer)` plus `resolved` when
                    // the unresolved entry carries one (independent-review
                    // findings on PR #163, rounds 2 and 3):
                    //
                    // - Two dependencies can share one `SourceTag` (e.g.
                    //   two hooks in the same `ClaudeUserSettings` file),
                    //   so matching on `source` alone would let one
                    //   dependency's genuinely confirmed-missing status get
                    //   masked into `Unverified` by an unrelated entry's
                    //   `unresolved` record for the same source.
                    //   `SourceUnreadable` carries no pointer at all — the
                    //   whole file failed to read, which blocks every
                    //   pointer from that source, so it still matches by
                    //   source alone (`u.pointer().is_none()`).
                    // - Two dependencies can ALSO share one exact `(source,
                    //   pointer)` with different `resolved` paths (see the
                    //   `.find()` comment above). `UnresolvedRef::resolved()`
                    //   returns `Some` only for `RestatFailed`, which is
                    //   scoped to one dependency's own re-stat — so when it
                    //   is present, it must also match `dep.resolved`, or a
                    //   sibling's restat failure would mask this
                    //   dependency's genuinely confirmed `Missing` into
                    //   `Unverified`. Every other variant carries no
                    //   `resolved` (`None`), meaning "matches any resolved
                    //   path at this pointer" — unchanged from round 2.
                    if post_report.unresolved.iter().any(|u| {
                        u.source() == &dep.source
                            && u.pointer().is_none_or(|p| p == dep.pointer)
                            && u.resolved().is_none_or(|r| r == dep.resolved)
                    }) {
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

/// Re-stat seam for one already-resolved dependency path (ADR-0001 §4.4).
/// Never walks a symlink chain and never reads a host config file — see
/// the module header for why a full `probe()` re-run is wrong here. The
/// production implementation ([`FsDependencyRestat`]) is a direct
/// `fs::symlink_metadata` call; tests inject a fixed table instead of
/// touching the real filesystem.
pub trait DependencyRestat {
    fn restat(&self, resolved: &Path) -> RestatOutcome;
}

/// What re-stating one previously-known dependency path found.
pub enum RestatOutcome {
    /// The path still resolves to a filesystem entry with this identity —
    /// may or may not match the pre-action identity; that comparison is
    /// [`compare_host_integrity`]'s job, not this type's.
    Present(crate::evidence::model::ExeIdentity),
    /// Confirmed absent (`ENOENT`/equivalent) — the only outcome
    /// [`restat_post_action`] reads as "genuinely gone".
    Missing,
    /// The stat itself failed for any other reason (permission denied,
    /// I/O error, or an injected failure standing in for "the health
    /// probe did not complete" — the "crash before health probe"
    /// scenario). Never read as a confirmed negative.
    Failed,
}

/// Production [`DependencyRestat`]: a direct `fs::symlink_metadata` call.
#[cfg(unix)]
pub struct FsDependencyRestat;

#[cfg(unix)]
impl DependencyRestat for FsDependencyRestat {
    fn restat(&self, resolved: &Path) -> RestatOutcome {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(resolved) {
            Ok(meta) => RestatOutcome::Present(crate::evidence::model::ExeIdentity {
                path: resolved.to_path_buf(),
                dev: meta.dev(),
                ino: meta.ino(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RestatOutcome::Missing,
            Err(_) => RestatOutcome::Failed,
        }
    }
}

/// Builds the POST-action [`ProbeOutcome<ExecutableDependencyReport>`]
/// that [`compare_host_integrity`] compares `pre` against, by re-stat'ing
/// only the dependencies `pre` already found — see the module header for
/// why this must not re-run the full discovery probe.
///
/// A dependency confirmed [`RestatOutcome::Missing`] is simply omitted
/// from the returned report's `references_inside` (and NOT recorded in
/// `unresolved` either) — `compare_host_integrity` reads exactly that
/// shape as a confirmed `Degraded`. A dependency whose re-stat
/// [`RestatOutcome::Failed`] is recorded as an [`UnresolvedRef::RestatFailed`]
/// entry carrying THIS dependency's own `(source, pointer)` — never
/// [`UnresolvedRef::SourceUnreadable`] (independent-review finding on PR
/// #163): that variant's `pointer()` is `None` by design, meaning "the
/// whole config file failed, every pointer from this source is
/// unresolved" — reusing it for a single dependency's own restat
/// failure would silently widen that claim and mask a genuinely missing
/// SIBLING dependency (same `SourceTag`, different pointer) into
/// `Unverified` instead of the correct `Degraded`. A dependency still
/// [`RestatOutcome::Present`] is carried forward with its freshly
/// observed identity, whether or not that identity still matches —
/// `compare_host_integrity` does that comparison.
pub fn restat_post_action(
    pre: &ExecutableDependencyReport,
    restat: &dyn DependencyRestat,
) -> ProbeOutcome<ExecutableDependencyReport> {
    let mut references_inside = Vec::with_capacity(pre.references_inside.len());
    let mut unresolved = Vec::new();

    for dep in &pre.references_inside {
        match restat.restat(&dep.resolved) {
            RestatOutcome::Present(exe) => {
                references_inside.push(DependencyRef { exe, ..dep.clone() });
            }
            RestatOutcome::Missing => {}
            RestatOutcome::Failed => {
                unresolved.push(UnresolvedRef::RestatFailed {
                    source: dep.source.clone(),
                    pointer: dep.pointer.clone(),
                    resolved: dep.resolved.clone(),
                });
            }
        }
    }

    ProbeOutcome::Observed(ExecutableDependencyReport {
        references_inside,
        running_inside: Vec::new(),
        unresolved,
        sources_examined: pre.sources_examined.clone(),
    })
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

    /// Independent-review finding on PR #163: two dependencies sharing one
    /// `SourceTag` (e.g. two hooks in the same `ClaudeUserSettings` file,
    /// at different structural pointers) must not let one's genuinely
    /// confirmed `Missing` get masked into `Unverified` by an unrelated
    /// `unresolved` entry for the SAME source but a DIFFERENT pointer.
    /// The matching rule is `(source, pointer)`, not `source` alone.
    #[test]
    fn unresolved_entry_for_a_different_pointer_does_not_mask_a_genuinely_missing_sibling() {
        let genuinely_missing = DependencyRef {
            pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            ..dep_ref(1)
        };
        let unrelated_unresolvable = DependencyRef {
            pointer: "hooks.PostToolUse[0].hooks[1]".to_string(),
            ..dep_ref(2)
        };
        let pre = clean(vec![
            genuinely_missing.clone(),
            unrelated_unresolvable.clone(),
        ]);
        let post = ProbeOutcome::Observed(ExecutableDependencyReport {
            // `genuinely_missing` is simply gone; `unrelated_unresolvable`
            // is still there (survives unchanged), but THIS source has a
            // separate unresolved entry for the OTHER pointer.
            references_inside: vec![unrelated_unresolvable.clone()],
            running_inside: Vec::new(),
            unresolved: vec![UnresolvedRef::MalformedSchema {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[1]".to_string(),
            }],
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let report = compare_host_integrity(&pre, &post);

        let missing_entry = report
            .entries
            .iter()
            .find(|e| e.pointer == genuinely_missing.pointer)
            .expect("the missing dependency must still be reported");
        assert_eq!(
            missing_entry.status,
            IntegrityStatus::Degraded,
            "an unresolved entry for a DIFFERENT pointer on the same source must not mask \
             a genuinely confirmed-missing dependency into Unverified"
        );
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

    // --- Real-filesystem integration tests for `restat_post_action` /
    // `FsDependencyRestat` -----------------------------------------------
    //
    // `references_inside` is unconditionally `PROTECTED` by
    // `classify` step 2b, so an *executed* action's pre-action snapshot
    // can never carry one in production — `execute()`'s own
    // class/reason-widening revalidation aborts first (see
    // `crate::executor::execute`'s module docs). The tests below prove
    // the underlying re-stat MECHANISM is correct against a real
    // filesystem (never a mock), independent of that policy-reachability
    // question, which is addressed in the PR description rather than
    // here.

    fn real_temp_dir(prefix: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-host-integrity-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        std::fs::create_dir_all(&dir).expect("create real temp dir");
        dir
    }

    fn real_dep_ref(path: &std::path::Path) -> DependencyRef {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).expect("fixture file must exist");
        DependencyRef {
            source: SourceTag::ClaudeUserSettings,
            pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            resolved: path.to_path_buf(),
            exe: crate::evidence::model::ExeIdentity {
                path: path.to_path_buf(),
                dev: meta.dev(),
                ino: meta.ino(),
            },
            provenance: Provenance::Observed,
            next_invocation: true,
        }
    }

    /// AC negative control, real filesystem: a disposable hook-dependency
    /// target whose binary is deleted (standing in for a bad `cargo clean
    /// --target-dir` sweep) must record DEGRADED via the real
    /// `fs::symlink_metadata`-based re-stat, not via any fake.
    #[test]
    fn real_fs_disposable_hook_target_missing_binary_is_degraded() {
        let root = real_temp_dir("missing");
        let hook_path = root.join("hook-binary");
        std::fs::write(&hook_path, b"#!/bin/sh\necho hook\n").unwrap();

        let dep = real_dep_ref(&hook_path);
        let pre = clean(vec![dep]);
        let pre_report = pre.observed().unwrap().clone();

        // The disposable-target cleanup deletes the binary alongside
        // everything else in the cache.
        std::fs::remove_file(&hook_path).unwrap();

        let post = restat_post_action(&pre_report, &FsDependencyRestat);
        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Degraded);
        assert_eq!(report.entries[0].status, IntegrityStatus::Degraded);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A restat double that returns a fixed [`RestatOutcome`] per path —
    /// used only where [`FsDependencyRestat`] cannot express the needed
    /// case (a generic I/O error, as opposed to a confirmed `ENOENT`).
    struct FixedRestat(std::collections::HashMap<PathBuf, &'static str>);

    impl DependencyRestat for FixedRestat {
        fn restat(&self, resolved: &std::path::Path) -> RestatOutcome {
            match self.0.get(resolved).copied() {
                Some("missing") => RestatOutcome::Missing,
                Some("failed") => RestatOutcome::Failed,
                Some(_) | None => panic!("no fixture entry for {}", resolved.display()),
            }
        }
    }

    /// Independent-review finding on PR #163 (round 2): `restat_post_action`
    /// must give a per-dependency restat failure its OWN `(source,
    /// pointer)` -- never [`UnresolvedRef::SourceUnreadable`], whose
    /// pointer-less shape would silently widen "this one dependency's
    /// restat failed" into "every dependency from this source is
    /// unresolved", masking a genuinely missing SIBLING into
    /// `Unverified`. Two dependencies share `SourceTag::ClaudeUserSettings`
    /// at different pointers: one restats `Failed`, the other restats
    /// `Missing`. The missing one must still report `Degraded`.
    #[test]
    fn a_sibling_dependencys_restat_failure_does_not_mask_a_genuinely_missing_one() {
        let failed_dep = DependencyRef {
            pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            resolved: PathBuf::from("/resource/bin/flaky-stat"),
            ..real_dep_ref_literal("/resource/bin/flaky-stat")
        };
        let missing_dep = DependencyRef {
            pointer: "hooks.PostToolUse[0].hooks[1]".to_string(),
            resolved: PathBuf::from("/resource/bin/gone"),
            ..real_dep_ref_literal("/resource/bin/gone")
        };
        let pre_report = clean(vec![failed_dep.clone(), missing_dep.clone()])
            .observed()
            .unwrap()
            .clone();
        let pre = ProbeOutcome::Observed(pre_report.clone());

        let mut table = std::collections::HashMap::new();
        table.insert(failed_dep.resolved.clone(), "failed");
        table.insert(missing_dep.resolved.clone(), "missing");
        let restat = FixedRestat(table);

        let post = restat_post_action(&pre_report, &restat);
        let report = compare_host_integrity(&pre, &post);

        let missing_entry = report
            .entries
            .iter()
            .find(|e| e.pointer == missing_dep.pointer)
            .expect("the missing dependency must still be reported");
        assert_eq!(
            missing_entry.status,
            IntegrityStatus::Degraded,
            "a sibling dependency's own restat failure must not mask a genuinely \
             missing dependency into Unverified"
        );
        let failed_entry = report
            .entries
            .iter()
            .find(|e| e.pointer == failed_dep.pointer)
            .expect("the failed-restat dependency must still be reported");
        assert_eq!(failed_entry.status, IntegrityStatus::Unverified);
        assert_eq!(report.status, IntegrityStatus::Degraded);
    }

    /// Independent-review finding on PR #163 (round 3): the exact masking
    /// bug round 2 "fixed" reproduces one level deeper. `resolve_command`
    /// can push multiple `DependencyRef`s sharing one exact `(source,
    /// pointer)` — e.g. a command whose head and an absolute-path argument
    /// both resolve inside the resource — so `(source, pointer)` is not a
    /// unique per-dependency key. Two dependencies here share BOTH source
    /// AND pointer, differing only by `resolved`: one restats `Failed`,
    /// the other restats `Missing`. The missing one must still report
    /// `Degraded`, not get masked into `Unverified` by its sibling's
    /// restat failure.
    ///
    /// Locates each entry by `pre_resolved`, not `pointer` — both entries
    /// share the pointer here, so looking up by pointer alone would find
    /// whichever entry happens to come first and make this test vacuous.
    #[test]
    fn a_sibling_with_the_same_pointer_but_a_different_resolved_path_does_not_mask_a_genuinely_missing_one(
    ) {
        let failed_dep = DependencyRef {
            resolved: PathBuf::from("/resource/bin/head-flaky-stat"),
            ..real_dep_ref_literal("/resource/bin/head-flaky-stat")
        };
        let missing_dep = DependencyRef {
            resolved: PathBuf::from("/resource/bin/arg-gone"),
            ..real_dep_ref_literal("/resource/bin/arg-gone")
        };
        assert_eq!(
            failed_dep.pointer, missing_dep.pointer,
            "this test's premise is that both dependencies share one exact \
             (source, pointer) — only `resolved` differs"
        );
        let pre_report = clean(vec![failed_dep.clone(), missing_dep.clone()])
            .observed()
            .unwrap()
            .clone();
        let pre = ProbeOutcome::Observed(pre_report.clone());

        let mut table = std::collections::HashMap::new();
        table.insert(failed_dep.resolved.clone(), "failed");
        table.insert(missing_dep.resolved.clone(), "missing");
        let restat = FixedRestat(table);

        let post = restat_post_action(&pre_report, &restat);
        let report = compare_host_integrity(&pre, &post);

        let missing_entry = report
            .entries
            .iter()
            .find(|e| e.pre_resolved == missing_dep.resolved)
            .expect("the missing dependency must still be reported");
        assert_eq!(
            missing_entry.status,
            IntegrityStatus::Degraded,
            "a sibling sharing the SAME (source, pointer) but a DIFFERENT \
             resolved path must not mask a genuinely missing dependency \
             into Unverified"
        );
        let failed_entry = report
            .entries
            .iter()
            .find(|e| e.pre_resolved == failed_dep.resolved)
            .expect("the failed-restat dependency must still be reported");
        assert_eq!(failed_entry.status, IntegrityStatus::Unverified);
        assert_eq!(report.status, IntegrityStatus::Degraded);
    }

    /// Secondary mis-pairing (round-3 independent review of PR #163,
    /// safe-direction but same root cause): the post-dependency lookup in
    /// `compare_host_integrity` used to `.find()` on `(source, pointer)`
    /// alone, so when two dependencies share one exact `(source,
    /// pointer)`, it could grab the WRONG sibling's post-action entry.
    /// Here both survive restat `Present`, unchanged — matching on
    /// `(source, pointer)` alone would still compare each pre-dependency
    /// against whichever post-entry `.find()` happens to return first,
    /// which is fine only by accident when both are unchanged; this test
    /// pins that both are correctly paired with THEIR OWN post entry by
    /// `resolved`, not an arbitrary one.
    #[test]
    fn two_siblings_with_the_same_pointer_are_each_compared_against_their_own_post_entry() {
        let dep_one = DependencyRef {
            resolved: PathBuf::from("/resource/bin/head-unchanged"),
            ..real_dep_ref_literal("/resource/bin/head-unchanged")
        };
        let dep_two = DependencyRef {
            resolved: PathBuf::from("/resource/bin/arg-unchanged"),
            exe: crate::evidence::model::ExeIdentity {
                path: PathBuf::from("/resource/bin/arg-unchanged"),
                dev: 1,
                ino: 99,
            },
            ..real_dep_ref_literal("/resource/bin/arg-unchanged")
        };
        assert_eq!(dep_one.pointer, dep_two.pointer);
        assert_ne!(
            dep_one.exe, dep_two.exe,
            "distinct (dev, ino) per sibling is what would expose a \
             wrong-sibling mis-pairing as a false Degraded"
        );

        let pre = clean(vec![dep_one.clone(), dep_two.clone()]);
        let post = clean(vec![dep_one.clone(), dep_two.clone()]);

        let report = compare_host_integrity(&pre, &post);

        assert_eq!(
            report.status,
            IntegrityStatus::Ok,
            "both siblings are genuinely unchanged; a wrong-sibling \
             mis-pairing on (source, pointer) alone would falsely degrade one"
        );
        for entry in &report.entries {
            assert_eq!(entry.status, IntegrityStatus::Ok);
        }
    }

    /// Literal `DependencyRef` builder for a fixed, non-existent path —
    /// unlike `real_dep_ref`, this never touches the real filesystem,
    /// which is what the `FixedRestat`-backed test above needs.
    fn real_dep_ref_literal(path: &str) -> DependencyRef {
        DependencyRef {
            source: SourceTag::ClaudeUserSettings,
            pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            resolved: PathBuf::from(path),
            exe: crate::evidence::model::ExeIdentity {
                path: PathBuf::from(path),
                dev: 1,
                ino: 2,
            },
            provenance: Provenance::Observed,
            next_invocation: true,
        }
    }

    /// AC: a valid, unchanged hook (same path, same real dev/ino on both
    /// sides) stays OK through the real re-stat mechanism.
    #[test]
    fn real_fs_valid_unchanged_hook_is_ok() {
        let root = real_temp_dir("unchanged");
        let hook_path = root.join("hook-binary");
        std::fs::write(&hook_path, b"#!/bin/sh\necho hook\n").unwrap();

        let dep = real_dep_ref(&hook_path);
        let pre = clean(vec![dep]);
        let pre_report = pre.observed().unwrap().clone();

        // Nothing touches the hook binary this time.
        let post = restat_post_action(&pre_report, &FsDependencyRestat);
        let report = compare_host_integrity(&pre, &post);

        assert_eq!(report.status, IntegrityStatus::Ok);
        assert_eq!(report.entries[0].status, IntegrityStatus::Ok);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A re-stat failure that is NOT a confirmed absence (simulated here
    /// via a dependency path whose PARENT directory was removed, which on
    /// most platforms still yields `NotFound` — so this additionally
    /// covers the "stale/dangling path" shape) must still never resolve
    /// to `Ok`. Covered precisely by `unresolvable_post_stat_is_unverified_never_ok`-
    /// style fixed-table tests above; this real-fs test only confirms
    /// `FsDependencyRestat` itself classifies a missing parent directory
    /// as `Missing`, not `Failed` or a panic.
    #[test]
    fn real_fs_restat_of_a_path_whose_parent_dir_is_gone_does_not_panic() {
        let root = real_temp_dir("parent-gone");
        let hook_path = root.join("subdir").join("hook-binary");
        std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
        std::fs::write(&hook_path, b"x").unwrap();
        let dep = real_dep_ref(&hook_path);

        std::fs::remove_dir_all(&root).unwrap();

        let outcome = FsDependencyRestat.restat(&dep.resolved);
        assert!(matches!(outcome, RestatOutcome::Missing));
    }
}
