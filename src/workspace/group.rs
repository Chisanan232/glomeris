//! Grouping discovered candidates into git worktree families, and the
//! branch survey that supplies each worktree's explanatory state.
//!
//! Pure, apart from [`WorkspaceSurvey::survey`], which is the one function
//! here that runs subprocesses. Read [`crate::workspace`]'s header before
//! adding anything: this module may not decide anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::branch::{BranchProbe, WorktreeBranchState};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::evidence::Evidence;
use crate::policy::PolicyDecision;
use crate::reporting::{label_for, PolicyLabel};

/// Per-worktree branch state, read once per worktree root.
///
/// Built separately from the grouping so the grouping itself stays pure:
/// a caller that cannot or should not run `git` passes
/// [`Self::unsurveyed`] and every worktree reports its branch state as
/// unavailable — never as a tidy default, which would claim a detached
/// HEAD with no upstream and no merge answer.
#[derive(Debug, Default)]
pub struct WorkspaceSurvey {
    by_root: BTreeMap<PathBuf, ProbeOutcome<WorktreeBranchState>>,
}

impl WorkspaceSurvey {
    /// A survey nobody ran. Every lookup reports `NotAttempted`.
    pub fn unsurveyed() -> Self {
        Self::default()
    }

    /// Probes each root once. `timeout` is per subprocess, as in
    /// [`crate::evidence::correlate::ProbeBudget`].
    ///
    /// Roots are deduplicated by the map, so a repository with forty
    /// discovered build directories under one worktree is asked once.
    pub fn survey<I>(roots: I, probe: &dyn BranchProbe, timeout: Duration) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut by_root = BTreeMap::new();
        for root in roots {
            by_root
                .entry(root)
                .or_insert_with_key(|root: &PathBuf| probe.state_of(root, timeout));
        }
        Self { by_root }
    }

    /// Visible to the rest of [`crate::workspace`] — [`super::graph`] needs
    /// the same lookup — and to nothing outside it. A survey that answered
    /// `NotAttempted` for a root it was never given is only safe because
    /// every caller sits inside the no-authority zone; widening this to
    /// `pub` would hand a deciding layer a branch fact.
    pub(super) fn state_of(&self, root: &Path) -> ProbeOutcome<WorktreeBranchState> {
        self.by_root
            .get(root)
            .cloned()
            .unwrap_or(ProbeOutcome::Unavailable(ProbeReason::NotAttempted))
    }
}

/// Whether something is using a worktree right now.
///
/// Three states. `Unknown` exists because the probes that answer this can
/// fail, and a failed `lsof` is not an idle worktree — see
/// [`Self::may_be_in_use`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    /// A process has a member open, is working inside it, or the owning
    /// tool is running.
    InUse,
    /// Every probe answered, and none of them found anything.
    Idle,
    /// At least one probe could not answer.
    Unknown,
}

impl ActivityState {
    /// `true` for `InUse` and for `Unknown`. The sentence this feeds is
    /// read by someone deciding what to let go of, and "idle" is the
    /// claim a failed probe cannot support.
    pub fn may_be_in_use(self) -> bool {
        !matches!(self, Self::Idle)
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::InUse => "in_use",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }
}

/// One discovered resource, as a member of a worktree.
///
/// Carries the resource id and the figures needed to total the family, and
/// deliberately carries nothing else. There is no `executable` field, no
/// action id and no offered action: a caller that wants to know what may
/// run looks the id up in the candidate list, where the action and the
/// refusal reason are. See [`crate::workspace`]'s header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMember {
    /// The same string [`crate::reporting::dto::DetectCandidateReport`]
    /// publishes, so the two lists join on it exactly.
    pub resource_id: String,
    /// `None` when nothing measured it. Never zero standing in for
    /// unmeasured — a family whose members' sizes are unknown must not
    /// total to a confident 0.
    pub reclaimable_bytes: Option<u64>,
    pub reclaimable_bytes_is_lower_bound: bool,
    /// The label the policy engine already assigned. Reported so the
    /// family can say how much of its bulk is behind a confirmation and
    /// how much is protected; not re-derived and not branched on to
    /// permit anything.
    pub label: PolicyLabel,
}

/// One git working tree and the discovered resources inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceWorktree {
    /// The working tree's own root — for a linked worktree, the sibling
    /// directory rather than the main checkout.
    pub root: PathBuf,
    /// `true` when this is a `git worktree add` sibling.
    pub linked: bool,
    pub dirty: bool,
    pub untracked: bool,
    pub activity: ActivityState,
    /// The newest `last_modified` any member reported, as the freshness
    /// signal campaign section 12 asks for. `None` when no member's
    /// mtime could be read.
    pub newest_member_modified_at: Option<SystemTime>,
    pub branch: ProbeOutcome<WorktreeBranchState>,
    pub members: Vec<WorkspaceMember>,
}

impl WorkspaceWorktree {
    /// Whether this worktree holds something that should stop a person
    /// from treating it as spent: uncommitted work, untracked files,
    /// something using it, or commits no remote has.
    ///
    /// A *statement*, not a gate. The executor never consults it; a
    /// worktree whose every signal here is quiet still reaches the policy
    /// engine and the deletion-time revalidation unchanged.
    pub fn holds_work_in_progress(&self) -> bool {
        self.dirty
            || self.untracked
            || self.activity.may_be_in_use()
            || match &self.branch {
                ProbeOutcome::Observed(state) => state.upstream.may_hold_unpushed_work(),
                // An unread branch is not a branch with nothing on it.
                ProbeOutcome::Unavailable(_) => true,
            }
    }
}

/// Every worktree sharing one git directory, i.e. one repository's
/// checkouts.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceFamily {
    /// The shared git directory, from
    /// [`crate::evidence::GitState::common_dir`]. The family's identity.
    pub common_dir: PathBuf,
    /// Sorted by root path, so two runs over the same disk produce the
    /// same order.
    pub worktrees: Vec<WorkspaceWorktree>,
}

impl WorkspaceFamily {
    pub fn worktree_count(&self) -> usize {
        self.worktrees.len()
    }

    pub fn members(&self) -> impl Iterator<Item = &WorkspaceMember> {
        self.worktrees.iter().flat_map(|w| w.members.iter())
    }
}

/// Groups candidates into worktree families.
///
/// Only candidates whose git state was *observed to exist* take part: a
/// resource outside any git working tree has no family, and one whose git
/// probe failed has no known family. Both are simply absent from the
/// result rather than collected into a synthetic "other" group, because a
/// group is a claim that its members belong together.
///
/// Pure. `survey` supplies the branch state; pass
/// [`WorkspaceSurvey::unsurveyed`] to group without running `git` again.
pub fn group_families(
    candidates: &[(Evidence, PolicyDecision)],
    survey: &WorkspaceSurvey,
) -> Vec<WorkspaceFamily> {
    // common_dir -> repo_root -> members, both ordered so the output is
    // deterministic for one disk state.
    let mut families: BTreeMap<PathBuf, BTreeMap<PathBuf, WorktreeAccumulator>> = BTreeMap::new();

    for (ev, decision) in candidates {
        let Some(Some(git)) = ev.git_state.observed() else {
            continue;
        };
        let reclaimable = ev.reclaimable_bytes.observed().copied();
        let accumulator = families
            .entry(git.common_dir.clone())
            .or_default()
            .entry(git.repo_root.clone())
            .or_insert_with(|| WorktreeAccumulator::new(git.worktree, git.dirty, git.untracked));
        accumulator.absorb(
            ev,
            WorkspaceMember {
                resource_id: ev.resource.to_string(),
                reclaimable_bytes: reclaimable,
                reclaimable_bytes_is_lower_bound: reclaimable.is_some()
                    && ev.reclaimable_bytes_is_lower_bound,
                label: label_for(decision),
            },
        );
    }

    families
        .into_iter()
        .map(|(common_dir, worktrees)| WorkspaceFamily {
            common_dir,
            worktrees: worktrees
                .into_iter()
                .map(|(root, accumulator)| accumulator.finish(root, survey))
                .collect(),
        })
        .collect()
}

/// Mutable state while one worktree's members are collected.
struct WorktreeAccumulator {
    linked: bool,
    dirty: bool,
    untracked: bool,
    any_in_use: bool,
    any_activity_unknown: bool,
    newest_member_modified_at: Option<SystemTime>,
    members: Vec<WorkspaceMember>,
}

impl WorktreeAccumulator {
    fn new(linked: bool, dirty: bool, untracked: bool) -> Self {
        Self {
            linked,
            dirty,
            untracked,
            any_in_use: false,
            any_activity_unknown: false,
            newest_member_modified_at: None,
            members: Vec::new(),
        }
    }

    fn absorb(&mut self, ev: &Evidence, member: WorkspaceMember) {
        match activity_of(ev) {
            ActivityState::InUse => self.any_in_use = true,
            ActivityState::Unknown => self.any_activity_unknown = true,
            ActivityState::Idle => {}
        }
        if let Some(modified) = ev.last_modified.observed() {
            self.newest_member_modified_at = Some(match self.newest_member_modified_at {
                Some(current) if current >= *modified => current,
                _ => *modified,
            });
        }
        self.members.push(member);
    }

    fn finish(self, root: PathBuf, survey: &WorkspaceSurvey) -> WorkspaceWorktree {
        // `InUse` outranks `Unknown`: one process holding a member open is
        // a settled answer for the worktree, whatever a second failed
        // probe elsewhere in it could not say.
        let activity = if self.any_in_use {
            ActivityState::InUse
        } else if self.any_activity_unknown {
            ActivityState::Unknown
        } else {
            ActivityState::Idle
        };
        WorkspaceWorktree {
            branch: survey.state_of(&root),
            root,
            linked: self.linked,
            dirty: self.dirty,
            untracked: self.untracked,
            activity,
            newest_member_modified_at: self.newest_member_modified_at,
            members: self.members,
        }
    }
}

/// Whether anything is using this resource, from the three correlation
/// probes that can say so.
///
/// Any probe that did not answer makes the result `Unknown`, unless
/// another one positively found use — which is a settled answer nothing
/// unread can withdraw.
fn activity_of(ev: &Evidence) -> ActivityState {
    let mut unknown = false;
    let mut in_use = false;

    match ev.open_by_process.observed() {
        Some(processes) => in_use |= !processes.is_empty(),
        None => unknown = true,
    }
    match ev.process_cwd_match.observed() {
        Some(processes) => in_use |= !processes.is_empty(),
        None => unknown = true,
    }
    match ev.tool_liveness.observed() {
        Some(running) => in_use |= *running,
        None => unknown = true,
    }

    if in_use {
        ActivityState::InUse
    } else if unknown {
        ActivityState::Unknown
    } else {
        ActivityState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        GitState, NativeCleanup, ProcessRef, Recoverability, Regenerability, ResourceFingerprint,
        ResourceKind, ResourceLocator,
    };
    use crate::evidence::{ProbeReason, ResourceId};
    use crate::policy::{PolicyClass, ReasonCode};
    use crate::workspace::branch::IntegrationEvidence;
    use crate::workspace::branch::{MergedState, UpstreamState};

    /// One worktree's git facts, as the probe would have reported them.
    struct Worktree {
        root: &'static str,
        common_dir: &'static str,
        linked: bool,
        dirty: bool,
        untracked: bool,
    }

    impl Worktree {
        fn clean(root: &'static str, common_dir: &'static str) -> Self {
            Self {
                root,
                common_dir,
                linked: true,
                dirty: false,
                untracked: false,
            }
        }

        fn dirty(mut self) -> Self {
            self.dirty = true;
            self
        }

        fn untracked(mut self) -> Self {
            self.untracked = true;
            self
        }

        fn main_checkout(mut self) -> Self {
            self.linked = false;
            self
        }

        fn state(&self) -> GitState {
            GitState {
                repo_root: PathBuf::from(self.root),
                common_dir: PathBuf::from(self.common_dir),
                dirty: self.dirty,
                untracked: self.untracked,
                worktree: self.linked,
            }
        }
    }

    fn evidence(path: &str, bytes: Option<u64>, worktree: &Worktree) -> Evidence {
        Evidence {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from(path)),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: bytes
                .map(ProbeOutcome::Observed)
                .unwrap_or(ProbeOutcome::Unavailable(ProbeReason::Failed)),
            physical_bytes: None,
            reclaimable_bytes: bytes
                .map(ProbeOutcome::Observed)
                .unwrap_or(ProbeOutcome::Unavailable(ProbeReason::Failed)),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::RegenerableByRebuild,
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(Some(worktree.state())),
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    fn decision(path: &str, class: PolicyClass) -> PolicyDecision {
        PolicyDecision {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from(path)),
            ),
            class,
            reasons: vec![ReasonCode::NoActiveUseObserved],
            evidence_collected_at: SystemTime::UNIX_EPOCH,
            evaluated_at: SystemTime::UNIX_EPOCH,
            policy_version: 1,
        }
    }

    fn candidate(
        path: &'static str,
        bytes: Option<u64>,
        worktree: &Worktree,
        class: PolicyClass,
    ) -> (Evidence, PolicyDecision) {
        (evidence(path, bytes, worktree), decision(path, class))
    }

    fn in_use(mut candidate: (Evidence, PolicyDecision)) -> (Evidence, PolicyDecision) {
        candidate.0.open_by_process =
            ProbeOutcome::Observed(vec![ProcessRef::new(42, "cargo".to_string())]);
        candidate
    }

    fn only_family(families: &[WorkspaceFamily]) -> &WorkspaceFamily {
        assert_eq!(families.len(), 1, "{families:#?}");
        &families[0]
    }

    /// AC 1: the family summary does not replace the members it summarizes.
    /// Every member is still individually present, under its own worktree,
    /// identified by the same string the candidate list publishes.
    #[test]
    fn grouping_keeps_every_member_identity() {
        let one = Worktree::clean("/work/app-feature", "/work/app/.git");
        let other = Worktree::clean("/work/app-fix", "/work/app/.git");
        let families = group_families(
            &[
                candidate(
                    "/work/app-feature/target",
                    Some(700),
                    &one,
                    PolicyClass::AutoSafe,
                ),
                candidate("/work/app-fix/target", Some(300), &other, PolicyClass::Ask),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        let family = only_family(&families);
        assert_eq!(family.common_dir, PathBuf::from("/work/app/.git"));
        assert_eq!(family.worktree_count(), 2);
        let ids: Vec<&str> = family.members().map(|m| m.resource_id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id.contains("/target")), "{ids:?}");
    }

    /// The grouping key is the shared git directory, not the working tree
    /// root — which is the whole point of HORO-1511's evidence change. Two
    /// sibling worktrees have different roots and are one family; two
    /// unrelated repositories are two families however similar their
    /// layout.
    #[test]
    fn siblings_are_one_family_and_unrelated_repos_are_not() {
        let app_a = Worktree::clean("/work/app", "/work/app/.git").main_checkout();
        let app_b = Worktree::clean("/work/app-feature", "/work/app/.git");
        let other = Worktree::clean("/work/other", "/work/other/.git").main_checkout();

        let families = group_families(
            &[
                candidate("/work/app/target", Some(1), &app_a, PolicyClass::AutoSafe),
                candidate(
                    "/work/app-feature/target",
                    Some(1),
                    &app_b,
                    PolicyClass::AutoSafe,
                ),
                candidate("/work/other/target", Some(1), &other, PolicyClass::AutoSafe),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(families.len(), 2);
        assert_eq!(families[0].common_dir, PathBuf::from("/work/app/.git"));
        assert_eq!(families[0].worktree_count(), 2);
        assert_eq!(families[1].common_dir, PathBuf::from("/work/other/.git"));
        assert_eq!(families[1].worktree_count(), 1);
    }

    /// AC 2: the aggregate reconciles with the members shown, by being
    /// nothing but them. Summed here from the same `Option<u64>` the
    /// candidate report publishes, so the two figures cannot disagree —
    /// and an unmeasured member stays visibly unmeasured rather than
    /// contributing a confident 0.
    #[test]
    fn aggregate_bytes_are_exactly_the_members_bytes() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let families = group_families(
            &[
                candidate(
                    "/work/app-feature/a",
                    Some(700),
                    &worktree,
                    PolicyClass::AutoSafe,
                ),
                candidate(
                    "/work/app-feature/b",
                    Some(300),
                    &worktree,
                    PolicyClass::AutoSafe,
                ),
                candidate(
                    "/work/app-feature/c",
                    None,
                    &worktree,
                    PolicyClass::AutoSafe,
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        let family = only_family(&families);
        let total: u64 = family.members().filter_map(|m| m.reclaimable_bytes).sum();
        assert_eq!(total, 1000);
        assert_eq!(family.members().count(), 3);
        assert_eq!(
            family
                .members()
                .filter(|m| m.reclaimable_bytes.is_none())
                .count(),
            1,
            "the unmeasured member must remain distinguishable from a zero-byte one"
        );
    }

    /// AC 6 and AC 3 together: one family of mixed members, where the
    /// merged/idle/clean worktree and the dirty, active and unpushed ones
    /// sit side by side. Each worktree answers for itself, and the group's
    /// overall look changes none of those answers.
    #[test]
    fn mixed_members_each_keep_their_own_answer() {
        let merged = Worktree::clean("/work/app-merged", "/work/app/.git");
        let unclean = Worktree::clean("/work/app-dirty", "/work/app/.git").dirty();
        let fresh = Worktree::clean("/work/app-new", "/work/app/.git").untracked();
        let busy = Worktree::clean("/work/app-busy", "/work/app/.git");

        let survey = WorkspaceSurvey {
            by_root: [
                (
                    PathBuf::from("/work/app-merged"),
                    ProbeOutcome::Observed(WorktreeBranchState {
                        branch: Some("done".to_string()),
                        upstream: UpstreamState::Tracking {
                            ahead: 0,
                            behind: 3,
                        },
                        merged: MergedState::Merged {
                            into: "origin/main".to_string(),
                        },
                        integration: IntegrationEvidence::not_attempted(),
                    }),
                ),
                (
                    PathBuf::from("/work/app-dirty"),
                    ProbeOutcome::Observed(WorktreeBranchState {
                        branch: Some("wip".to_string()),
                        upstream: UpstreamState::Tracking {
                            ahead: 0,
                            behind: 0,
                        },
                        merged: MergedState::Merged {
                            into: "origin/main".to_string(),
                        },
                        integration: IntegrationEvidence::not_attempted(),
                    }),
                ),
                (
                    PathBuf::from("/work/app-new"),
                    ProbeOutcome::Observed(WorktreeBranchState {
                        branch: Some("unpublished".to_string()),
                        upstream: UpstreamState::Untracked,
                        merged: MergedState::NotMerged {
                            into: "origin/main".to_string(),
                        },
                        integration: IntegrationEvidence::not_attempted(),
                    }),
                ),
                (
                    PathBuf::from("/work/app-busy"),
                    ProbeOutcome::Observed(WorktreeBranchState {
                        branch: Some("building".to_string()),
                        upstream: UpstreamState::Tracking {
                            ahead: 0,
                            behind: 0,
                        },
                        merged: MergedState::Merged {
                            into: "origin/main".to_string(),
                        },
                        integration: IntegrationEvidence::not_attempted(),
                    }),
                ),
            ]
            .into_iter()
            .collect(),
        };

        let families = group_families(
            &[
                candidate(
                    "/work/app-merged/target",
                    Some(9_000),
                    &merged,
                    PolicyClass::AutoSafe,
                ),
                candidate(
                    "/work/app-dirty/target",
                    Some(9_000),
                    &unclean,
                    PolicyClass::Ask,
                ),
                candidate(
                    "/work/app-new/target",
                    Some(9_000),
                    &fresh,
                    PolicyClass::Ask,
                ),
                in_use(candidate(
                    "/work/app-busy/target",
                    Some(9_000),
                    &busy,
                    PolicyClass::Protected,
                )),
            ],
            &survey,
        );

        let family = only_family(&families);
        assert_eq!(family.worktree_count(), 4);
        let by_root: BTreeMap<&str, &WorkspaceWorktree> = family
            .worktrees
            .iter()
            .map(|w| (w.root.to_str().expect("utf8 test path"), w))
            .collect();

        // The merged, clean, idle, fully-pushed one is the only member of
        // this family with nothing outstanding.
        let merged = by_root["/work/app-merged"];
        assert!(!merged.holds_work_in_progress());
        assert_eq!(merged.activity, ActivityState::Idle);

        // Uncommitted work. Its branch says "merged into origin/main",
        // which is exactly the reassurance that must not carry.
        let unclean = by_root["/work/app-dirty"];
        assert!(unclean.dirty);
        assert!(unclean.holds_work_in_progress());

        // Untracked files, and no upstream to have pushed them to.
        let fresh = by_root["/work/app-new"];
        assert!(fresh.untracked);
        assert!(fresh.holds_work_in_progress());

        // Something is using it right now.
        let busy = by_root["/work/app-busy"];
        assert_eq!(busy.activity, ActivityState::InUse);
        assert!(busy.holds_work_in_progress());

        // AC 3, stated as the count: three of the four stay outstanding
        // even though the family as a whole is 36 KiB of build output on a
        // repository whose default branch already contains most of it.
        assert_eq!(
            family
                .worktrees
                .iter()
                .filter(|w| w.holds_work_in_progress())
                .count(),
            3
        );
    }

    /// AC 3, structurally. A member carries the label the engine assigned
    /// and no permission of its own: the `Protected` member of an
    /// otherwise-safe family is still reported `Protected`, and there is no
    /// field on any of these types through which a group could say
    /// otherwise. (If someone adds one, this test's comment is the reason
    /// not to.)
    #[test]
    fn a_stale_looking_family_does_not_relabel_its_protected_member() {
        let merged = Worktree::clean("/work/app-old", "/work/app/.git");
        let busy = Worktree::clean("/work/app-busy", "/work/app/.git");
        let families = group_families(
            &[
                candidate("/work/app-old/a", Some(1), &merged, PolicyClass::AutoSafe),
                candidate("/work/app-old/b", Some(1), &merged, PolicyClass::AutoSafe),
                candidate("/work/app-old/c", Some(1), &merged, PolicyClass::AutoSafe),
                candidate("/work/app-busy/d", Some(1), &busy, PolicyClass::Protected),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        let family = only_family(&families);
        let protected: Vec<&WorkspaceMember> = family
            .members()
            .filter(|m| m.label == PolicyLabel::Protected)
            .collect();
        assert_eq!(protected.len(), 1);
        assert_eq!(protected[0].label, PolicyLabel::Protected);
    }

    /// A resource outside any git working tree, and one whose git probe
    /// failed, are both absent — not pooled into an unnamed group. A group
    /// is a claim that its members belong together, and neither of these
    /// supports it.
    #[test]
    fn resources_with_no_known_family_are_left_out() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let mut not_in_git = evidence("/tmp/loose", Some(5), &worktree);
        not_in_git.git_state = ProbeOutcome::Observed(None);
        let mut unprobed = evidence("/tmp/unknown", Some(5), &worktree);
        unprobed.git_state = ProbeOutcome::Unavailable(ProbeReason::TimedOut);

        let families = group_families(
            &[
                candidate(
                    "/work/app-feature/target",
                    Some(5),
                    &worktree,
                    PolicyClass::AutoSafe,
                ),
                (not_in_git, decision("/tmp/loose", PolicyClass::AutoSafe)),
                (unprobed, decision("/tmp/unknown", PolicyClass::AutoSafe)),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        let family = only_family(&families);
        assert_eq!(family.members().count(), 1);
    }

    /// An unsurveyed worktree reports its branch state as unavailable, and
    /// [`WorkspaceWorktree::holds_work_in_progress`] reads that as "may
    /// hold unpushed work". A clean, idle worktree nobody asked `git` about
    /// is therefore still outstanding — the fail-closed direction.
    #[test]
    fn an_unsurveyed_worktree_is_not_a_settled_one() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let families = group_families(
            &[candidate(
                "/work/app-feature/target",
                Some(1),
                &worktree,
                PolicyClass::AutoSafe,
            )],
            &WorkspaceSurvey::unsurveyed(),
        );

        let only = &only_family(&families).worktrees[0];
        assert_eq!(
            only.branch,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert!(!only.dirty);
        assert!(!only.untracked);
        assert_eq!(only.activity, ActivityState::Idle);
        assert!(
            only.holds_work_in_progress(),
            "an unread branch is not a branch with nothing on it"
        );
    }

    /// `InUse` outranks `Unknown` within a worktree: one process holding a
    /// member open is a settled answer, and a different member's failed
    /// probe cannot soften it. The reverse — `Unknown` winning — would
    /// report a demonstrably busy worktree as merely unverified.
    #[test]
    fn a_found_process_outranks_another_members_failed_probe() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let mut unreadable = evidence("/work/app-feature/b", Some(1), &worktree);
        unreadable.open_by_process = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);

        let families = group_families(
            &[
                in_use(candidate(
                    "/work/app-feature/a",
                    Some(1),
                    &worktree,
                    PolicyClass::Protected,
                )),
                (
                    unreadable,
                    decision("/work/app-feature/b", PolicyClass::AutoSafe),
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(
            only_family(&families).worktrees[0].activity,
            ActivityState::InUse
        );
    }

    /// A failed probe on its own leaves activity `Unknown`, which
    /// [`ActivityState::may_be_in_use`] counts as possible use. "Idle" is
    /// reserved for every probe having answered and found nothing.
    #[test]
    fn a_failed_probe_is_not_an_idle_worktree() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let mut unreadable = evidence("/work/app-feature/a", Some(1), &worktree);
        unreadable.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::Failed);

        let families = group_families(
            &[(
                unreadable,
                decision("/work/app-feature/a", PolicyClass::AutoSafe),
            )],
            &WorkspaceSurvey::unsurveyed(),
        );

        let only = &only_family(&families).worktrees[0];
        assert_eq!(only.activity, ActivityState::Unknown);
        assert!(only.activity.may_be_in_use());
        assert!(only.holds_work_in_progress());
    }

    /// AC 4's "the next rescan changes naturally": nothing is remembered
    /// between calls. A family is whatever this run's candidates say, so a
    /// finished worktree simply stops appearing — no stored profile of what
    /// a person usually deletes, and nothing to expire.
    #[test]
    fn a_finished_worktree_just_stops_appearing() {
        let kept = Worktree::clean("/work/app-keep", "/work/app/.git");
        let removed = Worktree::clean("/work/app-gone", "/work/app/.git");

        let before = group_families(
            &[
                candidate(
                    "/work/app-keep/target",
                    Some(1),
                    &kept,
                    PolicyClass::AutoSafe,
                ),
                candidate(
                    "/work/app-gone/target",
                    Some(1),
                    &removed,
                    PolicyClass::AutoSafe,
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );
        assert_eq!(only_family(&before).worktree_count(), 2);

        let after = group_families(
            &[candidate(
                "/work/app-keep/target",
                Some(1),
                &kept,
                PolicyClass::AutoSafe,
            )],
            &WorkspaceSurvey::unsurveyed(),
        );
        let family = only_family(&after);
        assert_eq!(family.worktree_count(), 1);
        assert_eq!(family.worktrees[0].root, PathBuf::from("/work/app-keep"));
    }

    /// Two runs over one disk state produce byte-identical structure, so a
    /// person watching this surface does not see families and worktrees
    /// reorder under them between rescans.
    #[test]
    fn output_order_does_not_depend_on_input_order() {
        let a = Worktree::clean("/work/app-a", "/work/app/.git");
        let b = Worktree::clean("/work/app-b", "/work/app/.git");
        let z = Worktree::clean("/work/zed", "/work/zed/.git").main_checkout();

        let forwards = group_families(
            &[
                candidate("/work/app-a/t", Some(1), &a, PolicyClass::AutoSafe),
                candidate("/work/app-b/t", Some(1), &b, PolicyClass::AutoSafe),
                candidate("/work/zed/t", Some(1), &z, PolicyClass::AutoSafe),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );
        let backwards = group_families(
            &[
                candidate("/work/zed/t", Some(1), &z, PolicyClass::AutoSafe),
                candidate("/work/app-b/t", Some(1), &b, PolicyClass::AutoSafe),
                candidate("/work/app-a/t", Some(1), &a, PolicyClass::AutoSafe),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(forwards, backwards);
    }

    /// The newest member's mtime is the family's freshness signal, and it
    /// survives a member whose mtime could not be read.
    #[test]
    fn freshness_is_the_newest_member_that_answered() {
        let worktree = Worktree::clean("/work/app-feature", "/work/app/.git");
        let recent = SystemTime::UNIX_EPOCH + Duration::from_secs(9_000);

        let mut newest = evidence("/work/app-feature/a", Some(1), &worktree);
        newest.last_modified = ProbeOutcome::Observed(recent);
        let mut unread = evidence("/work/app-feature/b", Some(1), &worktree);
        unread.last_modified = ProbeOutcome::Unavailable(ProbeReason::Failed);

        let families = group_families(
            &[
                (
                    unread,
                    decision("/work/app-feature/b", PolicyClass::AutoSafe),
                ),
                (
                    newest,
                    decision("/work/app-feature/a", PolicyClass::AutoSafe),
                ),
                candidate(
                    "/work/app-feature/c",
                    Some(1),
                    &worktree,
                    PolicyClass::AutoSafe,
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(
            only_family(&families).worktrees[0].newest_member_modified_at,
            Some(recent)
        );
    }

    /// The survey asks each root once however many members it holds, which
    /// is why the branch probe runs per root rather than per candidate.
    #[test]
    fn the_survey_asks_each_root_once() {
        use std::cell::RefCell;

        struct Counting(RefCell<Vec<PathBuf>>);
        impl BranchProbe for Counting {
            fn state_of(
                &self,
                worktree_root: &Path,
                _timeout: Duration,
            ) -> ProbeOutcome<WorktreeBranchState> {
                self.0.borrow_mut().push(worktree_root.to_path_buf());
                ProbeOutcome::Unavailable(ProbeReason::Failed)
            }
        }

        let probe = Counting(RefCell::new(Vec::new()));
        WorkspaceSurvey::survey(
            [
                PathBuf::from("/work/app-a"),
                PathBuf::from("/work/app-b"),
                PathBuf::from("/work/app-a"),
            ],
            &probe,
            Duration::from_secs(1),
        );

        assert_eq!(
            probe.0.into_inner(),
            vec![PathBuf::from("/work/app-a"), PathBuf::from("/work/app-b")]
        );
    }

    #[test]
    fn every_activity_state_has_a_distinct_tag() {
        let tags = [
            ActivityState::InUse.tag(),
            ActivityState::Idle.tag(),
            ActivityState::Unknown.tag(),
        ];
        assert_eq!(
            tags.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3
        );
    }
}
