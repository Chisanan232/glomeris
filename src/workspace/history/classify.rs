//! Turning a bounded set of observations into the one summary the rest of the
//! product is allowed to see (HORO-1547).
//!
//! # What is being classified, and what is not
//!
//! The shape of this machine's checkouts over time, and nothing about the
//! person. [`crate::workspace::WorkflowMode`]'s five words are descriptions of a
//! layout — one checkout, one checkout that moves between branches, several
//! worktrees at once, some of each, or not enough looked at to say. HORO-1547's
//! seventh acceptance criterion forbids labelling the user by skill or
//! competence, and this module is where such a label would have to be invented
//! for it to exist at all: there is no scale here, no ordering between the
//! modes, and none of them is the good one.
//! `scripts/check-workflow-history-labels-no-one.sh` holds that mechanically.
//!
//! # Why the window is applied here too
//!
//! [`super::store`] drops records outside [`super::store::RETENTION_WINDOW_SECS`]
//! when it writes, which bounds the file. It cannot bound *relevance*, because
//! nothing writes to a store on a machine nobody has run the recorder on for a
//! year, and those records would then still be classified as how this person
//! works today. So the window is applied again here, against the caller's `now`.
//! Two bounds, two different jobs; see that module's header.
//!
//! # Why a failed read is not an empty baseline
//!
//! [`classify`] maps the three [`super::store::StoreState`]s onto three
//! different answers, and only one of them is a summary. A store that was never
//! collected is `Unavailable(NotAttempted)`; one that could not be read is
//! `Unavailable` with the reason the read gave; one that was read is `Observed`,
//! even when what it holds is too little to claim anything — in which case the
//! summary itself says `unknown` / `insufficient` rather than this function
//! inventing an absence. That is HORO-1547's fifth acceptance criterion, and the
//! reason the three cases are not collapsed is the campaign's own rule: a failed
//! probe is not an answer of "nothing".

use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::workspace::graph::{WorkflowHistorySummary, WorkflowMode, WorkflowSupport};

use super::observation::WorkspaceObservation;
use super::store::{StoreState, RETENTION_WINDOW_SECS};

/// The one entry point: whatever the store said, plus the current time, into
/// the summary the graph carries.
pub fn classify(state: &StoreState, now_unix_secs: u64) -> ProbeOutcome<WorkflowHistorySummary> {
    match state {
        // Nothing has ever been recorded. Not a failure, and not an answer.
        StoreState::NeverCollected => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        // Something is there and could not be used. Reported as the read
        // reported it, so `failed` never arrives as `not_attempted`.
        StoreState::Unreadable(reason) => ProbeOutcome::Unavailable(*reason),
        StoreState::Collected(observations) => {
            ProbeOutcome::Observed(summarize(observations, now_unix_secs))
        }
    }
}

/// Summarises the observations inside the retention window.
///
/// Public so the classification can be tested without a file, and so a caller
/// holding observations already knows it does not have to write them down first
/// to ask what they mean.
pub fn summarize(
    observations: &[WorkspaceObservation],
    now_unix_secs: u64,
) -> WorkflowHistorySummary {
    let cutoff = now_unix_secs.saturating_sub(RETENTION_WINDOW_SECS);
    let within: Vec<&WorkspaceObservation> = observations
        .iter()
        .filter(|o| o.at_unix_secs >= cutoff)
        // An observation stamped in the future is a clock problem, not a look
        // at the machine. Counting it would let one bad stamp satisfy the
        // minimum on its own.
        .filter(|o| o.at_unix_secs <= now_unix_secs)
        .collect();

    let support = support_of(&within);
    let observed_mode = mode_of(&support);
    WorkflowHistorySummary::from_observations(observed_mode, within.len() as u32, support)
}

/// The shape of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Every repository looked at had one working tree.
    AllSerial,
    /// Every repository looked at had more than one.
    AllParallel,
    /// Some of each, at the same moment. Its own shape rather than a tie
    /// broken toward either: a person running six worktrees of one project and
    /// a single checkout of another is doing both, and calling that "parallel"
    /// would overstate what the other project's directory is for.
    Mixed,
    /// The observation looked at no repository at all — the recorder ran
    /// somewhere with nothing to see. Contributes to neither side.
    Nothing,
}

fn shape_of(observation: &WorkspaceObservation) -> Shape {
    if observation.repositories.is_empty() {
        return Shape::Nothing;
    }
    let parallel = observation
        .repositories
        .iter()
        .filter(|r| r.worktree_count > 1)
        .count();
    if parallel == 0 {
        Shape::AllSerial
    } else if parallel == observation.repositories.len() {
        Shape::AllParallel
    } else {
        Shape::Mixed
    }
}

/// The aggregate counts behind a classification — AC 4's "supporting evidence".
///
/// Computed before the mode rather than alongside it, because the mode is a
/// function of nothing else: `mode_of` reads only this, which is what makes the
/// support an explanation of the verdict rather than a separate story told
/// beside it.
fn support_of(observations: &[&WorkspaceObservation]) -> WorkflowSupport {
    let mut support = WorkflowSupport::nothing_observed();

    let mut repositories: std::collections::BTreeSet<_> = std::collections::BTreeSet::new();
    for observation in observations {
        match shape_of(observation) {
            Shape::AllSerial => support.serial_observations += 1,
            Shape::AllParallel => support.parallel_observations += 1,
            Shape::Mixed => support.mixed_observations += 1,
            Shape::Nothing => {}
        }
        for repo in &observation.repositories {
            repositories.insert(repo.repository);
            support.most_worktrees_seen_at_once =
                support.most_worktrees_seen_at_once.max(repo.worktree_count);
        }
    }
    support.repositories_observed = repositories.len() as u32;

    support.single_checkout_branch_changes = branch_changes(observations);

    if let (Some(first), Some(last)) = (observations.first(), observations.last()) {
        let span = last.at_unix_secs.saturating_sub(first.at_unix_secs);
        support.spanning_days = (span / (24 * 60 * 60)) as u32;
    }

    support
}

/// How many times a repository's single checkout was seen on a different branch
/// than the last time it was seen as a single checkout.
///
/// Compared per repository against its own previous *serial* sighting, not
/// against the immediately preceding observation: a project that goes single →
/// several worktrees → single again on a new branch did change branch in its
/// checkout, and a comparison that only looked at adjacent observations would
/// miss it.
///
/// A repository with no branch recorded — several worktrees, or a detached head
/// — contributes nothing and does not reset the comparison. The alternative
/// would count "I looked while it was detached" as a branch change.
fn branch_changes(observations: &[&WorkspaceObservation]) -> u32 {
    let mut last_seen: std::collections::BTreeMap<_, _> = std::collections::BTreeMap::new();
    let mut changes = 0;
    for observation in observations {
        for repo in &observation.repositories {
            let Some(branch) = repo.single_checkout_branch else {
                continue;
            };
            if let Some(previous) = last_seen.insert(repo.repository, branch) {
                if previous != branch {
                    changes += 1;
                }
            }
        }
    }
    changes
}

/// The mode the counts support, before the minimum-observations rule is applied
/// to it by [`WorkflowHistorySummary::from_observations`].
///
/// Deliberately not the place that decides whether there is enough evidence:
/// this answers "what shape do these counts describe", and the one constructor
/// of a summary answers "may that shape be claimed at all". Two rules in two
/// places, each testable on its own.
fn mode_of(support: &WorkflowSupport) -> WorkflowMode {
    let serial = support.serial_observations;
    let parallel = support.parallel_observations;
    let mixed = support.mixed_observations;

    if mixed > 0 || (serial > 0 && parallel > 0) {
        return WorkflowMode::Mixed;
    }
    if parallel > 0 {
        return WorkflowMode::ParallelMultiWorktree;
    }
    if serial > 0 {
        if support.single_checkout_branch_changes > 0 {
            return WorkflowMode::SerialMultiBranch;
        }
        return WorkflowMode::SerialSingleCheckout;
    }
    // No observation looked at a repository. Nothing was seen, so nothing is
    // claimed — and "unknown" is a real answer here rather than a default
    // persona, because there is no persona to fall back to.
    WorkflowMode::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::history::alias::LocalAlias;
    use crate::workspace::history::observation::RepositoryObservation;
    use crate::workspace::HistoryConfidence;
    use crate::workspace::MIN_OBSERVATIONS_FOR_A_PATTERN;

    const DAY: u64 = 24 * 60 * 60;
    const NOW: u64 = 1_800_000_000;

    fn repo(name: &str, worktrees: u32, branch: Option<&str>) -> RepositoryObservation {
        RepositoryObservation {
            repository: LocalAlias::of_name(name),
            worktree_count: worktrees,
            linked_worktree_count: worktrees.saturating_sub(1),
            detached_worktree_count: 0,
            single_checkout_branch: branch.map(LocalAlias::of_name),
        }
    }

    fn at(days_ago: u64, repositories: Vec<RepositoryObservation>) -> WorkspaceObservation {
        WorkspaceObservation {
            at_unix_secs: NOW - days_ago * DAY,
            repositories,
        }
    }

    /// Enough spaced observations to clear the minimum, all the same shape.
    fn enough(repositories: Vec<RepositoryObservation>) -> Vec<WorkspaceObservation> {
        (0..MIN_OBSERVATIONS_FOR_A_PATTERN as u64)
            .rev()
            .map(|d| at(d + 1, repositories.clone()))
            .collect()
    }

    fn summary(observations: &[WorkspaceObservation]) -> WorkflowHistorySummary {
        summarize(observations, NOW)
    }

    // -----------------------------------------------------------------
    // The four shapes
    // -----------------------------------------------------------------

    #[test]
    fn one_checkout_staying_on_one_branch_is_serial_single_checkout() {
        let s = summary(&enough(vec![repo("app", 1, Some("trunk"))]));
        assert_eq!(s.mode, WorkflowMode::SerialSingleCheckout);
        assert_eq!(s.confidence, HistoryConfidence::Observed);
        assert_eq!(s.support.single_checkout_branch_changes, 0);
    }

    /// The distinction no single snapshot can make, which is the whole reason
    /// the branch alias is stored at all.
    #[test]
    fn one_checkout_moving_between_branches_is_serial_multi_branch() {
        let observations = vec![
            at(3, vec![repo("app", 1, Some("trunk"))]),
            at(2, vec![repo("app", 1, Some("feature/a"))]),
            at(1, vec![repo("app", 1, Some("feature/b"))]),
        ];
        let s = summary(&observations);
        assert_eq!(s.mode, WorkflowMode::SerialMultiBranch);
        assert_eq!(s.support.single_checkout_branch_changes, 2);
    }

    #[test]
    fn several_worktrees_at_once_is_parallel_multi_worktree() {
        let s = summary(&enough(vec![repo("app", 5, None)]));
        assert_eq!(s.mode, WorkflowMode::ParallelMultiWorktree);
        assert_eq!(s.support.most_worktrees_seen_at_once, 5);
    }

    #[test]
    fn some_of_each_at_the_same_moment_is_mixed() {
        let s = summary(&enough(vec![
            repo("app", 4, None),
            repo("notes", 1, Some("trunk")),
        ]));
        assert_eq!(s.mode, WorkflowMode::Mixed);
        assert_eq!(s.support.mixed_observations, MIN_OBSERVATIONS_FOR_A_PATTERN);
    }

    #[test]
    fn some_moments_one_way_and_some_the_other_is_also_mixed() {
        let observations = vec![
            at(3, vec![repo("app", 1, Some("trunk"))]),
            at(2, vec![repo("app", 4, None)]),
            at(1, vec![repo("app", 1, Some("trunk"))]),
        ];
        assert_eq!(summary(&observations).mode, WorkflowMode::Mixed);
    }

    /// A project that went single checkout → worktrees → single checkout on a
    /// different branch did change branch. Comparing only adjacent observations
    /// would lose that, because the middle one records no branch.
    #[test]
    fn a_branch_change_across_a_parallel_spell_still_counts() {
        let observations = vec![
            at(3, vec![repo("app", 1, Some("trunk"))]),
            at(2, vec![repo("app", 3, None)]),
            at(1, vec![repo("app", 1, Some("feature/a"))]),
        ];
        assert_eq!(
            summary(&observations)
                .support
                .single_checkout_branch_changes,
            1
        );
    }

    /// A detached head has no branch to compare. Treating its absence as a
    /// change would report branch-hopping to someone who checked out a tag.
    #[test]
    fn a_detached_sighting_is_not_a_branch_change() {
        let observations = vec![
            at(3, vec![repo("app", 1, Some("trunk"))]),
            at(2, vec![repo("app", 1, None)]),
            at(1, vec![repo("app", 1, Some("trunk"))]),
        ];
        let s = summary(&observations);
        assert_eq!(s.support.single_checkout_branch_changes, 0);
        assert_eq!(s.mode, WorkflowMode::SerialSingleCheckout);
    }

    #[test]
    fn two_repositories_on_the_same_branch_name_are_not_one_repository() {
        let observations = vec![
            at(2, vec![repo("one", 1, Some("main"))]),
            at(1, vec![repo("two", 1, Some("main"))]),
        ];
        assert_eq!(summary(&observations).support.repositories_observed, 2);
    }

    // -----------------------------------------------------------------
    // AC 2: no claim from too little
    // -----------------------------------------------------------------

    /// The count comes from the store's spacing rule; this is the other half —
    /// even correctly spaced, too few observations may not be a habit.
    #[test]
    fn a_single_observation_is_never_a_pattern_whatever_it_shows() {
        let s = summary(&[at(1, vec![repo("app", 6, None)])]);
        assert_eq!(s.mode, WorkflowMode::Unknown);
        assert_eq!(s.confidence, HistoryConfidence::Insufficient);
        assert_eq!(s.observation_count, 1);
        assert_eq!(
            s.support.parallel_observations, 1,
            "the counts are still reported — they are what makes 'insufficient' auditable"
        );
    }

    #[test]
    fn one_short_of_the_minimum_is_still_insufficient() {
        let mut observations = enough(vec![repo("app", 1, Some("trunk"))]);
        observations.pop();
        let s = summary(&observations);
        assert_eq!(s.observation_count, MIN_OBSERVATIONS_FOR_A_PATTERN - 1);
        assert_eq!(s.confidence, HistoryConfidence::Insufficient);
        assert_eq!(s.mode, WorkflowMode::Unknown);
    }

    #[test]
    fn no_observations_at_all_is_unknown_and_insufficient() {
        let s = summary(&[]);
        assert_eq!(s.mode, WorkflowMode::Unknown);
        assert_eq!(s.confidence, HistoryConfidence::Insufficient);
        assert_eq!(s.observation_count, 0);
        assert_eq!(s.support, WorkflowSupport::nothing_observed());
    }

    /// An observation that looked at no repository is not a look at a machine
    /// with no repositories on it — it contributes to neither side, and enough
    /// of them still claim nothing.
    #[test]
    fn observations_that_saw_no_repository_support_no_mode() {
        let s = summary(&enough(vec![]));
        assert_eq!(s.mode, WorkflowMode::Unknown);
        assert_eq!(s.support.serial_observations, 0);
        assert_eq!(s.support.parallel_observations, 0);
        assert_eq!(
            s.observation_count, MIN_OBSERVATIONS_FOR_A_PATTERN,
            "the looks happened, and saying so is not the same as claiming a shape"
        );
    }

    // -----------------------------------------------------------------
    // The window
    // -----------------------------------------------------------------

    #[test]
    fn observations_older_than_the_window_do_not_count() {
        let mut observations = vec![at(
            RETENTION_WINDOW_SECS / DAY + 1,
            vec![repo("app", 9, None)],
        )];
        observations.extend(enough(vec![repo("app", 1, Some("trunk"))]));
        observations.sort_by_key(|o| o.at_unix_secs);
        let s = summary(&observations);
        assert_eq!(s.observation_count, MIN_OBSERVATIONS_FOR_A_PATTERN);
        assert_eq!(
            s.mode,
            WorkflowMode::SerialSingleCheckout,
            "a way of working from outside the window was reported as current practice"
        );
        assert_eq!(s.support.most_worktrees_seen_at_once, 1);
    }

    /// One observation stamped in the future must not satisfy the minimum on
    /// its own, and must not stretch the reported span to a decade.
    #[test]
    fn an_observation_stamped_in_the_future_does_not_count() {
        let observations = vec![WorkspaceObservation {
            at_unix_secs: NOW + 10 * 365 * DAY,
            repositories: vec![repo("app", 9, None)],
        }];
        let s = summary(&observations);
        assert_eq!(s.observation_count, 0);
        assert_eq!(s.support.spanning_days, 0);
    }

    #[test]
    fn the_span_is_the_days_between_the_oldest_and_newest_that_counted() {
        let observations = vec![
            at(10, vec![repo("app", 1, Some("trunk"))]),
            at(5, vec![repo("app", 1, Some("trunk"))]),
            at(1, vec![repo("app", 1, Some("trunk"))]),
        ];
        assert_eq!(summary(&observations).support.spanning_days, 9);
    }

    // -----------------------------------------------------------------
    // AC 5: the three store outcomes stay three
    // -----------------------------------------------------------------

    #[test]
    fn a_store_that_was_never_collected_is_not_attempted() {
        assert_eq!(
            classify(&StoreState::NeverCollected, NOW),
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    #[test]
    fn a_store_that_could_not_be_read_keeps_its_own_reason() {
        for reason in [
            ProbeReason::Failed,
            ProbeReason::PermissionDenied,
            ProbeReason::TimedOut,
        ] {
            assert_eq!(
                classify(&StoreState::Unreadable(reason), NOW),
                ProbeOutcome::Unavailable(reason),
                "an unreadable store was reported as something other than {}",
                reason.tag()
            );
        }
    }

    /// The distinction that makes AC 5 more than a slogan: a store that was
    /// read and holds too little is *observed* to be insufficient, which is a
    /// different fact from never having looked, and a reader can tell.
    #[test]
    fn a_collected_but_thin_store_is_observed_and_says_insufficient() {
        let state = StoreState::Collected(vec![at(1, vec![repo("app", 1, Some("trunk"))])]);
        match classify(&state, NOW) {
            ProbeOutcome::Observed(summary) => {
                assert_eq!(summary.confidence, HistoryConfidence::Insufficient);
                assert_eq!(summary.observation_count, 1);
            }
            other => panic!("a readable store became {other:?}"),
        }
    }

    /// Not a persona. There is no fallback shape, no default, and no ordering
    /// in which one of these modes is the one a user gets assigned when the
    /// evidence runs out.
    #[test]
    fn no_failure_path_produces_a_mode_other_than_unknown() {
        let failures = [
            StoreState::NeverCollected,
            StoreState::Unreadable(ProbeReason::Failed),
            StoreState::Unreadable(ProbeReason::PermissionDenied),
            StoreState::Collected(Vec::new()),
        ];
        for state in failures {
            match classify(&state, NOW) {
                ProbeOutcome::Unavailable(_) => {}
                ProbeOutcome::Observed(summary) => assert_eq!(
                    summary.mode,
                    WorkflowMode::Unknown,
                    "a failure path produced the mode {:?}",
                    summary.mode
                ),
            }
        }
    }
}
