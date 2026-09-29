//! One dated look at the shape of this machine's checkouts (HORO-1547).
//!
//! # What an observation is
//!
//! Per repository: how many working trees there are, how many of them are
//! `git worktree add` siblings, how many are on a detached head, and — only
//! when there is exactly one working tree — an opaque alias for the branch it
//! has checked out. Nothing else, and in particular no path and no branch name;
//! see [`super::alias`].
//!
//! That is the smallest set of facts that can distinguish the four shapes
//! HORO-1547 names. Three of them are visible in a single look:
//! `parallel_multi_worktree` is more than one working tree, `mixed` is some
//! repositories one way and some the other. The fourth is not:
//! `serial_single_checkout` and `serial_multi_branch` are the same picture at
//! any one instant, because a single checkout has exactly one branch whichever
//! of the two the person does. Only a comparison between observations tells
//! them apart, which is what the branch alias is for and the only reason it is
//! kept.
//!
//! # Why the census and not the worktree families
//!
//! [`crate::workspace::group_families`] already groups discovered resources by
//! repository, and reusing it here would have cost no subprocess at all. It
//! would also have been wrong: a family exists only where a *detector found
//! something*, so a working tree nobody has built in yet is simply absent from
//! it — `a_finished_worktree_just_stops_appearing` in that module asserts
//! exactly this, as a feature. Counting worktrees that way yields a lower bound,
//! and a lower bound of one is indistinguishable from a single checkout. The
//! baseline would then report serial development to someone running six
//! worktrees, which is the confident-wrong answer this campaign exists to avoid.
//!
//! So the count comes from `git worktree list`, which is authoritative about
//! the question being asked, costs one invocation per repository, and answers
//! even for a repository in which no detector found a byte.
//!
//! # Why current activity is deliberately absent
//!
//! An observation records *shape*, not *use*. It would have been easy to also
//! store how many working trees had a process in them, and HORO-1547 lists
//! overlapping activity among the signals worth considering. It is left out on
//! purpose, for a reason stronger than cost:
//!
//! Activity is the fact that must always be read fresh. Section 12 of the
//! campaign is explicit that current evidence always wins and that a historical
//! pattern must never make an active worktree reclaimable — and the surest way
//! to break that is to keep a remembered copy of the very field the live probe
//! answers, one file lookup away from whoever is ranking. A number that is not
//! stored cannot be consulted in place of today's. Today's activity reaches the
//! model through [`crate::workspace::ActivityFacts`], every run, with its own
//! unanswered-probe count attached.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::evidence::correlate::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

use super::alias::LocalAlias;

/// One repository's shape, at one moment.
///
/// Every count is of working trees, so `worktree_count` is always at least 1
/// for a repository that appears at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepositoryObservation {
    /// Which repository, as an equality token. The alias of its shared git
    /// directory, so two working trees of one repository agree and two
    /// repositories that both have a `main` do not.
    pub repository: LocalAlias,
    pub worktree_count: u32,
    /// How many of those are `git worktree add` siblings — i.e.
    /// `worktree_count - 1` whenever the main checkout is among them.
    pub linked_worktree_count: u32,
    /// How many are on a detached head. Recorded because a detached head has
    /// no branch to compare across observations, so a repository that is
    /// always detached must not be read as a checkout that never switched.
    pub detached_worktree_count: u32,
    /// The branch of the single working tree, when there is exactly one and it
    /// is on a branch. `None` in every other case, including a repository with
    /// several working trees — the branch question only distinguishes the two
    /// serial shapes, and a repository with siblings has already answered a
    /// different question.
    pub single_checkout_branch: Option<LocalAlias>,
}

/// One dated look at every repository the recorder was pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceObservation {
    /// Seconds since the Unix epoch. Seconds, not a date, and used only for
    /// admission spacing and retention — see [`super::store`].
    pub at_unix_secs: u64,
    /// Sorted by alias, so two runs over the same disk produce the same bytes
    /// and a stored observation can be compared field by field in a test.
    pub repositories: Vec<RepositoryObservation>,
}

impl WorkspaceObservation {
    /// Builds an observation from an already-taken census.
    ///
    /// Pure, so the classification tests never run `git`. Repositories whose
    /// census failed are *absent* rather than present with a zero count: "I
    /// could not enumerate this repository's working trees" is not "this
    /// repository has none", and a zero would pull the whole machine's shape
    /// toward serial exactly when the evidence was missing.
    pub fn from_census(at_unix_secs: u64, entries: &[(PathBuf, CensusEntry)]) -> Self {
        let mut by_repository: BTreeMap<LocalAlias, Vec<&CensusEntry>> = BTreeMap::new();
        for (common_dir, entry) in entries {
            by_repository
                .entry(LocalAlias::of_path(common_dir))
                .or_default()
                .push(entry);
        }

        let repositories = by_repository
            .into_iter()
            .map(|(repository, worktrees)| {
                let worktree_count = worktrees.len() as u32;
                let linked_worktree_count = worktrees.iter().filter(|e| e.linked).count() as u32;
                let detached_worktree_count =
                    worktrees.iter().filter(|e| e.branch.is_none()).count() as u32;
                let single_checkout_branch = match worktrees.as_slice() {
                    [only] => only.branch.as_deref().map(LocalAlias::of_name),
                    _ => None,
                };
                RepositoryObservation {
                    repository,
                    worktree_count,
                    linked_worktree_count,
                    detached_worktree_count,
                    single_checkout_branch,
                }
            })
            .collect();

        Self {
            at_unix_secs,
            repositories,
        }
    }

    /// Takes a census of each root and builds one observation.
    ///
    /// A root whose census could not be taken contributes nothing — see
    /// [`Self::from_census`]. Two roots that turn out to be working trees of
    /// one repository collapse into one entry, because the census reports the
    /// shared git directory and that is what the alias is taken from.
    pub fn collect<I>(
        at_unix_secs: u64,
        roots: I,
        census: &dyn WorktreeCensus,
        timeout: Duration,
    ) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut seen: BTreeMap<PathBuf, Vec<CensusEntry>> = BTreeMap::new();
        for root in roots {
            // Asked once per *repository*, not once per root: several
            // configured project roots are routinely siblings of one
            // repository, and enumerating it once per sibling would multiply
            // the subprocess count by the number of worktrees — worst exactly
            // on the machines this feature is for.
            match census.repository_of(&root, timeout) {
                ProbeOutcome::Observed(common_dir) => {
                    if seen.contains_key(&common_dir) {
                        continue;
                    }
                    if let ProbeOutcome::Observed(entries) = census.worktrees_of(&root, timeout) {
                        seen.insert(common_dir, entries);
                    }
                }
                ProbeOutcome::Unavailable(_) => continue,
            }
        }

        let flattened: Vec<(PathBuf, CensusEntry)> = seen
            .into_iter()
            .flat_map(|(common_dir, entries)| {
                entries
                    .into_iter()
                    .map(move |entry| (common_dir.clone(), entry))
            })
            .collect();
        Self::from_census(at_unix_secs, &flattened)
    }
}

/// One working tree, as the census saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusEntry {
    /// Kept only long enough to be counted. Never stored — the observation
    /// holds aliases and counts.
    pub root: PathBuf,
    /// `true` for a `git worktree add` sibling. `git worktree list` prints the
    /// main checkout first, which is how this is decided.
    pub linked: bool,
    /// `None` for a detached head. A bare repository's entry also has no
    /// branch; it also has no working tree anybody edits, and it is counted
    /// the same way rather than specially, because either way there is no
    /// branch to compare across observations.
    pub branch: Option<String>,
}

/// Enumerates a repository's working trees.
///
/// Two methods rather than one because [`WorkspaceObservation::collect`] needs
/// the repository's identity *before* deciding whether to pay for the
/// enumeration — see the comment there.
pub trait WorktreeCensus {
    /// The shared git directory of the repository containing `root`.
    ///
    /// `Unavailable` for a path that is not in a repository at all, which is an
    /// ordinary outcome for a configured project root that is just a
    /// directory of projects.
    fn repository_of(&self, root: &Path, timeout: Duration) -> ProbeOutcome<PathBuf>;

    /// Every working tree of the repository containing `root`.
    ///
    /// `Unavailable(reason)` means the enumeration could not be made. It must
    /// not be turned into an empty list: see [`WorkspaceObservation::from_census`].
    fn worktrees_of(&self, root: &Path, timeout: Duration) -> ProbeOutcome<Vec<CensusEntry>>;
}

/// Real [`WorktreeCensus`], backed by the `git` CLI.
pub struct GitCliWorktreeCensus;

impl WorktreeCensus for GitCliWorktreeCensus {
    fn repository_of(&self, root: &Path, timeout: Duration) -> ProbeOutcome<PathBuf> {
        // `--path-format=absolute` because the default is relative to the
        // process's working directory, and this process's working directory has
        // nothing to do with the root being asked about. A relative answer
        // would still alias — to a different token per caller, which is the
        // silent version of the bug.
        match run_git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            timeout,
        ) {
            Ok(output) if output.status.success() => {
                let text = trimmed(&output.stdout);
                if text.is_empty() {
                    ProbeOutcome::Unavailable(ProbeReason::Failed)
                } else {
                    ProbeOutcome::Observed(PathBuf::from(text))
                }
            }
            // Not a repository. A non-zero exit here is git answering, not git
            // failing, so it is not reported as a failure.
            Ok(_) => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            Err(reason) => ProbeOutcome::Unavailable(reason),
        }
    }

    fn worktrees_of(&self, root: &Path, timeout: Duration) -> ProbeOutcome<Vec<CensusEntry>> {
        match run_git(root, &["worktree", "list", "--porcelain"], timeout) {
            Ok(output) if output.status.success() => {
                match parse_worktree_list(&String::from_utf8_lossy(&output.stdout)) {
                    // An empty list would mean git listed no working trees for
                    // a repository it just acknowledged, which cannot happen
                    // and would be recorded as a repository with a zero count
                    // if it were believed.
                    entries if entries.is_empty() => ProbeOutcome::Unavailable(ProbeReason::Failed),
                    entries => ProbeOutcome::Observed(entries),
                }
            }
            Ok(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
            Err(reason) => ProbeOutcome::Unavailable(reason),
        }
    }
}

/// Parses `git worktree list --porcelain`.
///
/// Records are separated by a blank line and each begins with a `worktree`
/// line. Within a record, `branch refs/heads/<name>` names the branch and a
/// bare `detached` line says there is none. Newer git adds `bare`, `locked`
/// and `prunable` lines, which are ignored rather than rejected — an unknown
/// attribute must not discard a record whose `worktree` line parsed.
///
/// The first record is the main checkout; every later one is linked. That is
/// git's documented order, and it is the only way the porcelain form
/// distinguishes them.
fn parse_worktree_list(stdout: &str) -> Vec<CensusEntry> {
    let mut entries: Vec<CensusEntry> = Vec::new();
    let mut root: Option<PathBuf> = None;
    let mut branch: Option<String> = None;

    // Closure would need two mutable borrows; a small helper keeps the loop
    // readable and the flush in one place.
    //
    // No separate reset of `branch`, because the loop below only sets it while a
    // record is open — so taking it here is the only way it is ever cleared, and
    // a branch cannot leak from one record into the next.
    macro_rules! flush {
        () => {
            if let Some(root) = root.take() {
                entries.push(CensusEntry {
                    root,
                    linked: !entries.is_empty(),
                    branch: branch.take(),
                });
            }
        };
    }

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // A `worktree` line opens a record. Flushing here rather than only
            // on the blank separator means a truncated final record — output
            // cut off mid-stream — still counts, instead of being dropped as
            // if the working tree did not exist.
            flush!();
            root = Some(PathBuf::from(path));
        } else if let Some(reference) = line.strip_prefix("branch ") {
            // Only while a record is open. A `branch` line before any
            // `worktree` line belongs to no working tree, and keeping it would
            // attach it to whichever record opened next.
            if root.is_some() {
                branch = Some(short_branch(reference).to_string());
            }
        }
        // `detached` needs no arm: `branch` is already `None`.
    }
    flush!();

    entries
}

/// `refs/heads/main` → `main`. Any other ref form is kept whole rather than
/// trimmed by a fixed number of segments, so a ref shape this function has not
/// seen aliases consistently instead of aliasing to a fragment.
fn short_branch(reference: &str) -> &str {
    reference.strip_prefix("refs/heads/").unwrap_or(reference)
}

fn trimmed(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_string()
}

fn run_git(
    path: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, ProbeReason> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    match run_with_timeout(command, timeout) {
        CommandOutcome::NotFound => Err(ProbeReason::ToolAbsent),
        CommandOutcome::TimedOut => Err(ProbeReason::TimedOut),
        CommandOutcome::SpawnFailed => Err(ProbeReason::Failed),
        CommandOutcome::Completed(output) => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str, branch: Option<&str>) -> CensusEntry {
        CensusEntry {
            root: PathBuf::from(root),
            // Set by the parser in production; irrelevant to the counts this
            // helper's callers assert, which is why they pass it explicitly
            // where it matters.
            linked: false,
            branch: branch.map(str::to_string),
        }
    }

    fn linked(mut entry: CensusEntry) -> CensusEntry {
        entry.linked = true;
        entry
    }

    // -----------------------------------------------------------------
    // Parsing
    // -----------------------------------------------------------------

    #[test]
    fn the_first_record_is_the_main_checkout_and_the_rest_are_linked() {
        let parsed = parse_worktree_list(
            "worktree /p/app\nHEAD abc\nbranch refs/heads/main\n\
             \n\
             worktree /p/app-feature\nHEAD def\nbranch refs/heads/feature/x\n",
        );
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].linked);
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(parsed[1].linked);
        assert_eq!(parsed[1].branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn a_detached_record_has_no_branch_rather_than_a_placeholder_one() {
        let parsed = parse_worktree_list("worktree /p/app\nHEAD abc\ndetached\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].branch, None);
    }

    /// `bare`, `locked` and `prunable` are attributes git added after this
    /// parser was written, and the next one will be too. A record must survive
    /// an attribute this function does not know.
    #[test]
    fn an_unrecognised_attribute_does_not_discard_the_record() {
        let parsed = parse_worktree_list(
            "worktree /p/app\nHEAD abc\nbranch refs/heads/main\nlocked\nprunable gitdir gone\n",
        );
        assert_eq!(parsed.len(), 1, "the record was dropped by an attribute");
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
    }

    /// Output truncated mid-record, which is what a killed `git` leaves.
    /// Dropping the last record would undercount worktrees, and undercounting
    /// is the direction that reads as serial development.
    #[test]
    fn a_record_with_no_trailing_blank_line_still_counts() {
        let parsed =
            parse_worktree_list("worktree /p/app\nbranch refs/heads/main\n\nworktree /p/app-two");
        assert_eq!(parsed.len(), 2);
        assert!(parsed[1].linked);
        assert_eq!(parsed[1].branch, None);
    }

    #[test]
    fn a_ref_that_is_not_under_refs_heads_is_kept_whole() {
        assert_eq!(short_branch("refs/heads/main"), "main");
        assert_eq!(short_branch("refs/bisect/bad"), "refs/bisect/bad");
    }

    // -----------------------------------------------------------------
    // Derivation
    // -----------------------------------------------------------------

    #[test]
    fn one_checkout_on_a_branch_records_that_branch() {
        let observation = WorkspaceObservation::from_census(
            1_000,
            &[(PathBuf::from("/p/app/.git"), entry("/p/app", Some("main")))],
        );
        let repo = &observation.repositories[0];
        assert_eq!(repo.worktree_count, 1);
        assert_eq!(repo.linked_worktree_count, 0);
        assert_eq!(
            repo.single_checkout_branch,
            Some(LocalAlias::of_name("main"))
        );
    }

    /// The branch is only ever asked about to tell the two serial shapes
    /// apart. A repository with siblings is already a different shape, and
    /// storing a branch for it would be identity kept for no question.
    #[test]
    fn several_checkouts_record_no_branch_at_all() {
        let observation = WorkspaceObservation::from_census(
            1_000,
            &[
                (PathBuf::from("/p/app/.git"), entry("/p/app", Some("main"))),
                (
                    PathBuf::from("/p/app/.git"),
                    linked(entry("/p/app-x", Some("feature/x"))),
                ),
            ],
        );
        let repo = &observation.repositories[0];
        assert_eq!(repo.worktree_count, 2);
        assert_eq!(repo.linked_worktree_count, 1);
        assert_eq!(repo.single_checkout_branch, None);
    }

    #[test]
    fn a_detached_single_checkout_has_no_branch_to_compare() {
        let observation = WorkspaceObservation::from_census(
            1_000,
            &[(PathBuf::from("/p/app/.git"), entry("/p/app", None))],
        );
        let repo = &observation.repositories[0];
        assert_eq!(repo.detached_worktree_count, 1);
        assert_eq!(
            repo.single_checkout_branch, None,
            "a detached head aliased to something would read as a branch that never changed"
        );
    }

    #[test]
    fn two_repositories_stay_two_even_on_the_same_branch_name() {
        let observation = WorkspaceObservation::from_census(
            1_000,
            &[
                (PathBuf::from("/p/one/.git"), entry("/p/one", Some("main"))),
                (PathBuf::from("/p/two/.git"), entry("/p/two", Some("main"))),
            ],
        );
        assert_eq!(observation.repositories.len(), 2);
    }

    #[test]
    fn repositories_come_out_in_a_stable_order() {
        let forwards = WorkspaceObservation::from_census(
            1_000,
            &[
                (PathBuf::from("/p/one/.git"), entry("/p/one", Some("main"))),
                (PathBuf::from("/p/two/.git"), entry("/p/two", Some("main"))),
            ],
        );
        let backwards = WorkspaceObservation::from_census(
            1_000,
            &[
                (PathBuf::from("/p/two/.git"), entry("/p/two", Some("main"))),
                (PathBuf::from("/p/one/.git"), entry("/p/one", Some("main"))),
            ],
        );
        assert_eq!(forwards, backwards);
    }

    // -----------------------------------------------------------------
    // Collection: a failed census is not an empty repository
    // -----------------------------------------------------------------

    struct FakeCensus {
        /// root -> (common dir, worktrees). Absent means "not a repository".
        repositories: Vec<(PathBuf, PathBuf, ProbeOutcome<Vec<CensusEntry>>)>,
        asked: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl WorktreeCensus for FakeCensus {
        fn repository_of(&self, root: &Path, _timeout: Duration) -> ProbeOutcome<PathBuf> {
            match self.repositories.iter().find(|(r, _, _)| r == root) {
                Some((_, common_dir, _)) => ProbeOutcome::Observed(common_dir.clone()),
                None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            }
        }

        fn worktrees_of(&self, root: &Path, _timeout: Duration) -> ProbeOutcome<Vec<CensusEntry>> {
            self.asked.borrow_mut().push(root.to_path_buf());
            match self.repositories.iter().find(|(r, _, _)| r == root) {
                Some((_, _, outcome)) => outcome.clone(),
                None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            }
        }
    }

    fn census(repositories: Vec<(PathBuf, PathBuf, ProbeOutcome<Vec<CensusEntry>>)>) -> FakeCensus {
        FakeCensus {
            repositories,
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The load-bearing honesty property of this file. A repository whose
    /// enumeration failed must be absent, not present with one working tree —
    /// a count of one is the shape of a single checkout, so believing a
    /// failure here would manufacture serial evidence out of a missing answer.
    #[test]
    fn a_repository_whose_census_failed_is_absent_not_single_checkout() {
        let census = census(vec![(
            PathBuf::from("/p/app"),
            PathBuf::from("/p/app/.git"),
            ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        )]);
        let observation = WorkspaceObservation::collect(
            1_000,
            vec![PathBuf::from("/p/app")],
            &census,
            Duration::from_secs(1),
        );
        assert!(
            observation.repositories.is_empty(),
            "a timed-out enumeration became a repository with {} working trees",
            observation.repositories[0].worktree_count
        );
    }

    #[test]
    fn a_root_that_is_not_a_repository_is_skipped_without_asking_for_a_list() {
        let census = census(vec![]);
        let observation = WorkspaceObservation::collect(
            1_000,
            vec![PathBuf::from("/p/not-a-repo")],
            &census,
            Duration::from_secs(1),
        );
        assert!(observation.repositories.is_empty());
        assert!(
            census.asked.borrow().is_empty(),
            "the enumeration ran for a path that is not a repository"
        );
    }

    /// Several configured roots that are working trees of one repository.
    /// Enumerating once per root would multiply the subprocess cost by the
    /// worktree count, worst on exactly the machines this feature is for.
    #[test]
    fn sibling_roots_of_one_repository_are_enumerated_once() {
        let entries = vec![
            entry("/p/app", Some("main")),
            linked(entry("/p/app-x", Some("feature/x"))),
        ];
        let census = census(vec![
            (
                PathBuf::from("/p/app"),
                PathBuf::from("/p/app/.git"),
                ProbeOutcome::Observed(entries.clone()),
            ),
            (
                PathBuf::from("/p/app-x"),
                PathBuf::from("/p/app/.git"),
                ProbeOutcome::Observed(entries),
            ),
        ]);
        let observation = WorkspaceObservation::collect(
            1_000,
            vec![PathBuf::from("/p/app"), PathBuf::from("/p/app-x")],
            &census,
            Duration::from_secs(1),
        );
        assert_eq!(observation.repositories.len(), 1);
        assert_eq!(observation.repositories[0].worktree_count, 2);
        assert_eq!(
            census.asked.borrow().len(),
            1,
            "one repository was enumerated once per sibling root"
        );
    }

    /// Real `git`, because the porcelain format is the contract and a fixture
    /// string only proves this parser agrees with itself.
    #[test]
    fn the_real_census_answers_for_a_scratch_repository() {
        let root = super::super::fixtures::scratch_repo("census");
        let census = GitCliWorktreeCensus;
        let timeout = Duration::from_secs(20);

        let common_dir = match census.repository_of(&root, timeout) {
            ProbeOutcome::Observed(dir) => dir,
            ProbeOutcome::Unavailable(reason) => {
                panic!("git could not name the common dir: {}", reason.tag())
            }
        };
        assert!(
            common_dir.is_absolute(),
            "a relative common dir aliases differently per caller: {common_dir:?}"
        );

        match census.worktrees_of(&root, timeout) {
            ProbeOutcome::Observed(entries) => {
                assert_eq!(entries.len(), 1, "a fresh repo has one working tree");
                assert!(!entries[0].linked);
                assert!(
                    entries[0].branch.is_some(),
                    "the scratch repo has a commit on a branch, so the census must name one"
                );
            }
            ProbeOutcome::Unavailable(reason) => {
                panic!("git could not list worktrees: {}", reason.tag())
            }
        }
    }

    /// A path with no repository anywhere above it. `NotAttempted` rather than
    /// `Failed`: git answered the question, and the answer was no.
    #[test]
    fn a_path_outside_any_repository_is_not_a_failure() {
        let outside = std::env::temp_dir();
        // Only meaningful if the temp dir really is outside a repository;
        // on a machine where it is not, the assertion below would be about
        // something else entirely.
        if matches!(
            GitCliWorktreeCensus.repository_of(&outside, Duration::from_secs(20)),
            ProbeOutcome::Observed(_)
        ) {
            return;
        }
        assert!(matches!(
            GitCliWorktreeCensus.repository_of(&outside, Duration::from_secs(20)),
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        ));
    }
}
