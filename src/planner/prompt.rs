//! What the model is told, version 2 (HORO-1548).
//!
//! # This file is not a security boundary
//!
//! Every constraint stated here is also enforced in Rust, and the Rust is what
//! makes it true. A response that ignores all of it produces an empty
//! [`super::validate::ValidatedWorkspacePlan`] and a set of counters, not a
//! deletion. The prompt exists to make a *correct* answer likely and a
//! *useful* answer possible — [`super::response`] and [`super::validate`] make
//! an incorrect one harmless. Anyone tempted to move a rule out of those
//! modules and into this string should read
//! `scripts/check-workspace-aggregation-has-no-authority.sh` first.
//!
//! # Why the prompt is assembled rather than written out
//!
//! The word lists come from [`super::contract`] and
//! [`crate::workspace::WorkflowMode`], interpolated. A hand-written prompt
//! listing the four dispositions is a second copy of the vocabulary, and the
//! failure it eventually produces is the quiet kind: the parser learns a
//! fifth word, the prompt never mentions it, and the capability is shipped
//! unused for a year — or the reverse, where the prompt advertises a word
//! [`super::validate`] drops, and every response spends items on something
//! that is silently discarded. Interpolating makes both impossible rather
//! than testable.

use super::contract::{
    ClaimConfidence, Disposition, ObservationKind, ProbeId, ProbeSubjectKind,
    PLANNER_CONTRACT_VERSION,
};
use crate::workspace::WorkflowMode;

/// The role. Deliberately not "storage cleanup ranking assistant", which is
/// what version 1 said: a ranker sorts a list it is handed, and the whole
/// point of the workspace projection is that the list is not the interesting
/// part. What this model is for is reading a developer's workspace — several
/// checkouts of one repository, a coding agent running in one of them, a
/// build directory that has not been touched in a month — and saying which
/// parts of it a human can safely stop storing, and which parts nobody has
/// established anything about yet.
const ROLE: &str = "You are a Developer Workspace Recovery Analyst. You are given a \
privacy-safe projection of one developer machine: repositories, the working trees inside \
them, the branch and activity state of each, optional external context, the storage \
resources each contains, the global tool caches, and a bounded summary of how this \
machine has been used over time. Identities are withheld on purpose — you see opaque \
references like repo_1, workspace_2 and resource_3, never a path, a branch name, a \
repository name or a task title. Your job is to explain this workspace and say what can \
be reclaimed from it, in that order.";

/// The three-valued discipline, and then the six inferences that must not be
/// made. Each of the six is a real conflation a plausible model makes, and
/// each is separately tested in [`super::validate`] and, once HORO-1550
/// lands, separately visible in the GUI.
const EVIDENCE_DISCIPLINE: &str = "Every claim you make is one of three things, and you \
must say which: OBSERVED, meaning the projection states it; INFERRED, meaning you \
concluded it from what the projection states; or UNKNOWN, meaning the projection does not \
establish it. A value the projection reports as unavailable is UNKNOWN, and so is a probe \
that failed or was never attempted. Missing evidence is never negative evidence. \
Specifically: no pull-request data does not mean no pull request exists; no task \
association does not mean no active task exists; a failed process probe does not mean no \
process is using the resource; no upstream branch does not mean there is no unique local \
work; a task marked done does not mean anything is safe to delete; and a merged pull \
request does not mean the current working tree contains nothing newer. If you cannot tell, \
say UNKNOWN and ask for a probe. Guessing is worse than asking.";

/// Where the authority actually is. Phrased as what the model may do first,
/// because a list of prohibitions alone reads as an invitation to find the
/// gap.
const AUTHORITY: &str = "You may rank, explain, point out evidence that conflicts, infer \
how this developer works, recommend acting now or asking the user or deferring or keeping, \
and request more read-only evidence. You may not do anything else, and in particular: you \
may not authorize deletion, you may not assign or imply a policy class, you may not invent \
an action id, you may not invent a resource id, you may not name a filesystem path, you \
may not produce a shell command or an argument list, you may not turn something that needs \
asking or is protected into something executable, you may not decide that enough storage \
has been recovered, you may not change any budget, and you may not treat a historical \
pattern as permission. A recommendation from you is re-checked against freshly collected \
evidence before anything happens, so a recommendation that ignores these rules costs the \
user a round trip and achieves nothing.";

/// The two rules that decide what a *useful* answer looks like rather than a
/// permitted one, and that the aggregate-versus-member confusion is a real
/// failure mode worth naming: HORO-1511 exists because a group of stale
/// worktrees is not authority over the one dirty member of that group.
const JUDGEMENT: &str = "Current evidence outranks history. If a working tree is dirty, or \
has a process active in it, or holds commits that exist nowhere else, then no historical \
pattern and no external status makes its contents reclaimable. A group of resources never \
grants permission to any one of its members: judge each resource on its own evidence, even \
when every sibling points the other way. Do not describe the developer — not as advanced, \
beginner, expert, good or bad. Describe the workspace.";

/// Assembles the version 2 system prompt.
///
/// Rebuilt on each call rather than cached: it is a few hundred bytes
/// assembled once per provider request, and a `OnceLock` here would be a
/// synchronisation primitive guarding a `format!`.
pub fn system_prompt() -> String {
    format!(
        "{ROLE}\n\n\
         {EVIDENCE_DISCIPLINE}\n\n\
         {AUTHORITY}\n\n\
         {JUDGEMENT}\n\n\
         Respond with ONLY one JSON object, with no prose around it and no fields other \
         than these:\n\
         {{\"contract_version\": {PLANNER_CONTRACT_VERSION}, \
         \"workspace_profile\": {{\"mode\": one of [{modes}], \"confidence\": one of \
         [{confidences}], \"evidence_refs\": [reference], \"summary\": string}}, \
         \"items\": [{{\"resource_id\": reference, \"action_id\": one of the \
         offered_action_ids of that resource, \"disposition\": one of [{dispositions}], \
         \"confidence\": one of [{confidences}], \"priority\": number, \"evidence_refs\": \
         [reference], \"uncertainties\": [string], \"reason\": string}}], \
         \"observations\": [{{\"kind\": one of [{kinds}], \"evidence_refs\": [reference], \
         \"detail\": string}}], \
         \"evidence_requests\": [{{\"probe_id\": one of [{probes}], \"subject_ref\": \
         reference, \"reason\": string}}]}}\n\n\
         Each probe accepts only certain kinds of reference, and a request naming any other \
         kind is discarded even though the probe and the reference are both real: \
         {probe_subjects}. No probe accepts a machine or a repository reference: a repository \
         is not a working tree, and picking one of its working trees on your behalf is not \
         something this can do. Ask about the working tree you mean.\n\n\
         Every reference must be one you were given in this request. Every action id must \
         be one listed in that resource's own offered_action_ids; an action offered for a \
         different resource does not count. An unrecognised field anywhere makes the whole \
         response unreadable, and a word outside these lists is discarded. A probe request \
         names a probe and a subject and nothing else — there is no field for a command, a \
         path or a URL, and supplying one makes the request unreadable rather than \
         powerful.",
        modes = tag_list(&WorkflowMode::ALL.map(WorkflowMode::tag)),
        confidences = tag_list(&ClaimConfidence::ALL.map(ClaimConfidence::tag)),
        dispositions = tag_list(&Disposition::ALL.map(Disposition::tag)),
        kinds = tag_list(&ObservationKind::ALL.map(ObservationKind::tag)),
        probes = tag_list(&ProbeId::ALL.map(ProbeId::tag)),
        probe_subjects = probe_subjects(),
    )
}

/// `"git_branch_state" takes a worktree reference; ...` — one clause per probe,
/// read off [`ProbeId::subject_kinds`].
///
/// Generated for the reason the word lists are: this pairing is the one part of
/// the probe contract HORO-1548 left the model to guess at, and a hand-written
/// sentence describing it would be a second copy of a table
/// [`super::validate`] enforces. A model that guesses wrong spends a whole
/// round on a request that is dropped, which is a silent cost — the request was
/// well-formed, the probe was real, the reference was one we issued, and
/// nothing happened.
fn probe_subjects() -> String {
    ProbeId::ALL
        .iter()
        .map(|probe| {
            let kinds = probe
                .subject_kinds()
                .iter()
                .map(|kind| kind.tag())
                .collect::<Vec<_>>()
                .join(" or a ");
            format!("\"{}\" takes a {kinds} reference", probe.tag())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `"a", "b", "c"` — quoted, because every one of these is a JSON string
/// value and an unquoted list invites a response that omits the quotes.
fn tag_list(tags: &[&'static str]) -> String {
    tags.iter()
        .map(|tag| format!("\"{tag}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason the prompt is assembled: every word the parser accepts is
    /// offered to the model, and adding a variant to either side cannot
    /// silently skip the other.
    #[test]
    fn every_word_the_parser_accepts_is_offered_to_the_model() {
        let prompt = system_prompt();

        let vocabularies: Vec<(&str, Vec<&'static str>)> = vec![
            (
                "workflow mode",
                WorkflowMode::ALL.iter().map(|m| m.tag()).collect(),
            ),
            (
                "confidence",
                ClaimConfidence::ALL.iter().map(|c| c.tag()).collect(),
            ),
            (
                "disposition",
                Disposition::ALL.iter().map(|d| d.tag()).collect(),
            ),
            (
                "observation kind",
                ObservationKind::ALL.iter().map(|k| k.tag()).collect(),
            ),
            ("probe", ProbeId::ALL.iter().map(|p| p.tag()).collect()),
        ];

        for (vocabulary, tags) in vocabularies {
            assert!(!tags.is_empty(), "the {vocabulary} list is empty");
            for tag in tags {
                assert!(
                    prompt.contains(&format!("\"{tag}\"")),
                    "the {vocabulary} value {tag} is accepted but never offered"
                );
            }
        }
    }

    /// HORO-1549. Every probe is offered with every subject kind it accepts,
    /// so the pairing the parser enforces is the pairing the model is told.
    #[test]
    fn every_probe_is_offered_with_the_subject_kinds_it_accepts() {
        let prompt = system_prompt();

        for probe in ProbeId::ALL {
            let kinds = probe.subject_kinds();
            assert!(
                !kinds.is_empty(),
                "{} accepts no subject at all, so it can never be asked",
                probe.tag()
            );
            for kind in kinds {
                assert!(
                    prompt.contains(&format!("a {} reference", kind.tag()))
                        || prompt.contains(&format!("a {} or a ", kind.tag())),
                    "{} accepts a {} subject and the prompt never says so",
                    probe.tag(),
                    kind.tag()
                );
            }
            assert!(
                prompt.contains(&format!("\"{}\" takes a ", probe.tag())),
                "{} is listed as a probe but its subject kind is left to a guess",
                probe.tag()
            );
        }
    }

    /// The non-vacuous half. The two citable kinds no probe accepts are named
    /// as refused rather than simply left out — a model that reads an omission
    /// as permission is the reason `repo_1` was accepted by HORO-1548's check
    /// and is not by this one.
    ///
    /// Without this, the test above would pass against a prompt that offered
    /// every probe with every kind.
    #[test]
    fn the_two_kinds_no_probe_accepts_are_refused_in_words() {
        let prompt = system_prompt();

        for kind in [ProbeSubjectKind::Machine, ProbeSubjectKind::Repository] {
            assert!(
                ProbeId::ALL.iter().all(|probe| !probe.accepts(kind)),
                "{} is now accepted by some probe, so this test is out of date",
                kind.tag()
            );
            assert!(
                !prompt.contains(&format!("takes a {} reference", kind.tag())),
                "the prompt offers a probe a {} subject, which is always dropped",
                kind.tag()
            );
        }
        assert!(prompt.contains("No probe accepts a machine or a repository reference"));
        assert!(prompt.contains("a repository is not a working tree"));
    }

    /// §13. Each of the six is a conflation a plausible model makes, so each
    /// is refused in words as well as in code.
    #[test]
    fn the_prompt_refuses_each_way_missing_evidence_becomes_negative_evidence() {
        let prompt = system_prompt();

        for clause in [
            "no pull-request data does not mean no pull request exists",
            "no task association does not mean no active task exists",
            "a failed process probe does not mean no process is using the resource",
            "no upstream branch does not mean there is no unique local work",
            "a task marked done does not mean anything is safe to delete",
            "a merged pull request does not mean the current working tree contains nothing newer",
        ] {
            assert!(prompt.contains(clause), "the prompt does not say: {clause}");
        }

        assert!(prompt.contains("Missing evidence is never negative evidence"));
        assert!(prompt.contains("OBSERVED"));
        assert!(prompt.contains("INFERRED"));
        assert!(prompt.contains("UNKNOWN"));
    }

    /// §15. The prohibitions are all enforced in Rust; they are stated here
    /// too so a cooperative model does not waste a response discovering them.
    #[test]
    fn the_prompt_states_every_thing_the_model_may_not_do() {
        let prompt = system_prompt();

        for clause in [
            "may not authorize deletion",
            "may not assign or imply a policy class",
            "may not invent an action id",
            "may not invent a resource id",
            "may not name a filesystem path",
            "may not produce a shell command",
            "may not turn something that needs asking or is protected into something executable",
            "may not decide that enough storage has been recovered",
            "may not change any budget",
            "may not treat a historical pattern as permission",
        ] {
            assert!(prompt.contains(clause), "the prompt does not say: {clause}");
        }
    }

    /// §12 and §18. Two rules about what outranks what, both of which a model
    /// optimising for recovered bytes will otherwise get wrong.
    #[test]
    fn the_prompt_says_current_evidence_outranks_history_and_siblings() {
        let prompt = system_prompt();
        assert!(prompt.contains("Current evidence outranks history"));
        assert!(prompt.contains("never grants permission to any one of its members"));
        // §12's explicit list. The model is asked to describe a workspace, not
        // to grade the person using it.
        for grade in ["advanced", "beginner", "expert", "good or bad"] {
            assert!(
                prompt.contains(grade),
                "the prompt does not forbid describing the developer as {grade}"
            );
        }
    }

    /// The role, and that it is no longer version 1's. A prompt that still
    /// described a ranking assistant would produce ranked items and no
    /// profile, no observations and no probe requests — a v2-shaped response
    /// with only v1 content in it.
    #[test]
    fn the_role_is_an_analyst_rather_than_a_ranker() {
        let prompt = system_prompt();
        assert!(prompt.contains("Developer Workspace Recovery Analyst"));
        assert!(
            !prompt.contains("storage cleanup ranking assistant"),
            "the version 1 role survived into the version 2 prompt"
        );
        // The four things a v2 response carries that a v1 response could not.
        for shape in [
            "workspace_profile",
            "items",
            "observations",
            "evidence_requests",
        ] {
            assert!(prompt.contains(shape), "the prompt never asks for {shape}");
        }
    }

    #[test]
    fn the_prompt_declares_the_version_the_response_will_be_held_to() {
        let prompt = system_prompt();
        assert!(prompt.contains(&format!("\"contract_version\": {PLANNER_CONTRACT_VERSION}")));
    }

    /// The projection withholds every local identity, so the prompt must not
    /// reintroduce the idea that the model has one to work with.
    #[test]
    fn the_prompt_names_no_local_identity_and_asks_for_none() {
        let prompt = system_prompt();
        for leak in ["/Users/", "/home/", "$HOME", "~/", "git@", "https://"] {
            assert!(
                !prompt.contains(leak),
                "the prompt contains {leak}, which is a local identity shape"
            );
        }
        assert!(prompt.contains("never a path"));
    }

    #[test]
    fn the_prompt_is_the_same_string_every_time() {
        assert_eq!(system_prompt(), system_prompt());
    }
}
