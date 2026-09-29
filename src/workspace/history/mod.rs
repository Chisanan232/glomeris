//! A bounded local baseline for how this machine's checkouts have been laid out
//! over time (HORO-1547).
//!
//! # Why this exists
//!
//! [`crate::workspace::WorkflowHistorySummary`] arrived with HORO-1542 as
//! vocabulary with no producer: in production
//! [`crate::workspace::WorkspaceEvidenceGraph`]'s `history` field is
//! `Unavailable(NotAttempted)`, and only tests have ever set it. This module is
//! the producer. A single run cannot tell whether one checkout means serial
//! development or a parallel worker who happens to have closed everything, and
//! §12 of the campaign is blunt about it — a single snapshot cannot honestly
//! establish "usually". So observations are written down, spaced apart, and read
//! back together.
//!
//! # History has no authority
//!
//! Nothing here may make anything deletable. The summary this module produces is
//! an explanation of where storage came from and an input to ordering; it is not
//! evidence about any particular resource, and current evidence always wins. The
//! structural guarantees behind that claim, rather than the intention:
//!
//! - The summary reaches the graph as one field on the machine node and is not
//!   reachable from a [`crate::workspace::WorktreeNode`] at all, so no code
//!   ranking a worktree can consult a habit while looking at a fact.
//! - It holds only counts and a mode. There is no resource id, no path and no
//!   branch name in it, so it cannot be matched against the thing being
//!   considered for deletion even by a caller that wanted to.
//! - Policy classification never reads it. `workflow_history_cannot_make_a_
//!   resource_executable` in `tests/workflow_history_has_no_authority.rs`
//!   mutates the history between two otherwise identical runs and asserts the
//!   verdicts are byte-identical.
//!
//! # The three halves
//!
//! - [`observation`] — one dated look at the shape of the checkouts, from an
//!   authoritative `git worktree list` census rather than from whatever the
//!   detectors happened to find.
//! - [`store`] — the bounded file. Three bounds: how many records, how old, and
//!   how closely spaced two records may be. The last of those is what stops a
//!   `for` loop from manufacturing a pattern.
//! - [`classify`] — observations plus the current time into the one summary the
//!   rest of the product sees, including the three-way distinction between never
//!   collected, unreadable, and read-but-thin.
//!
//! [`alias`] underpins all three: the opaque equality token that lets the file
//! compare repositories and branches across time without keeping either.
//!
//! # What is deliberately not wired
//!
//! **Not recorded from `detect`.** The data an observation needs is in hand
//! there, so recording would be nearly free — but `detect` is declared
//! `Safety::ReadOnly` in [`crate::cli::help`], and this codebase's own
//! vocabulary makes writing own state `WritesOwnState` (cf. `settings`,
//! `daemon`). Reclassifying a read-only command is a user-visible contract
//! change, and quietly writing a file from one is worse. Recording is its own
//! command instead.
//!
//! **Not recorded from the daemon.** The poll loop has no worktree knowledge at
//! all; giving it some would add `git` subprocesses to a sixty-second tick.
//! That is a behaviour change this ticket does not ask for.

pub mod alias;
pub mod classify;
pub mod observation;
pub mod store;

#[cfg(test)]
mod fixtures;

pub use alias::LocalAlias;
pub use classify::{classify, summarize};
pub use observation::{
    CensusEntry, GitCliWorktreeCensus, RepositoryObservation, WorkspaceObservation, WorktreeCensus,
};
pub use store::{
    default_history_path, read, record, Admission, StoreState, FORMAT_VERSION, MAX_OBSERVATIONS,
    MIN_ADMISSION_INTERVAL_SECS, RETENTION_WINDOW_SECS,
};
