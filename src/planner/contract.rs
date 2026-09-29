//! The words a planner response is allowed to use, and the version that
//! says which set of them applies (HORO-1548).
//!
//! # Why the vocabularies are enums here rather than strings at the parse site
//!
//! Every type in this module exists to turn one of the model's strings into
//! one of a fixed set of local values, and to return `None` for anything
//! else. That is the whole of it. There is no leniency anywhere: no case
//! folding, no trimming, no hyphen/underscore equivalence, no prefix match,
//! no nearest neighbour — the same discipline
//! [`crate::evidence::ResourceKind::from_tag`] settled on, for the same
//! reason. `recommend` is not a shorter `recommend_now`, and a parser that
//! guessed it was would be deciding something on the model's behalf.
//!
//! Keeping them here, rather than as `#[derive(Deserialize)]` enums on the
//! claim structs in [`super::response`], buys one specific thing: an
//! unrecognised word can be handled *per field* instead of failing the whole
//! response. Serde's enum deserializer has exactly one behaviour for an
//! unknown variant — error — which would take a whole plan down over a
//! single mistyped confidence. [`super::validate`] makes that choice field by
//! field and documents each one; this module only says what the legal words
//! are.
//!
//! # The version
//!
//! [`PLANNER_CONTRACT_VERSION`] is sent in the request and expected back in
//! the response. It exists so that changing this contract later is a
//! migration rather than a silent reinterpretation of somebody's `--plan-file`
//! fixture: a response declaring a version this build does not implement is
//! refused outright, and a response declaring none is read as the one
//! contract that existed before versions were sent at all. See
//! [`super::response::ContractVersion`].

/// The version of the planner request/response contract this build speaks.
///
/// Version 1 is the ranking contract in [`crate::actions::llm`]: a flat array
/// of resource views out, `{"items": [{resource_id, action_id, priority,
/// reason}]}` back. It was never versioned on the wire, which is precisely
/// why this constant exists — the way to tell v1 from v2 is that v1 says
/// nothing.
///
/// Version 2 is this campaign's: the whole bounded workspace projection out
/// ([`super::dto::ModelGraphView`]), and a closed four-part response back.
pub const PLANNER_CONTRACT_VERSION: u32 = 2;

/// What the planner is recommending be done about one resource, *now*.
///
/// This is the model's advisory verdict and it is not a policy class. The
/// distinction is the campaign's central one and it is worth being exact
/// about: [`crate::policy::PolicyClass`] is what Glomeris will permit, decided
/// locally from evidence; a `Disposition` is what a model thinks is worth
/// doing about a resource, and it can be wrong about every one of them without
/// anything becoming executable. A `RecommendNow` on an `Ask` resource still
/// asks. A `RecommendNow` on a `Protected` resource is not rendered at all.
///
/// The four words are not a scale and are deliberately not ordered:
/// `AskUser` is not "half of `RecommendNow`", it is a different statement
/// about who should decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Worth reclaiming now, on the evidence as it stands.
    RecommendNow,
    /// A person should look at this. The model's way of saying the evidence
    /// is real but pulls in two directions — which is a more useful answer
    /// than a confident one, and the reason this variant exists rather than
    /// being folded into `Defer`.
    AskUser,
    /// Not now. Something knowable would change the answer.
    Defer,
    /// Leave it. The model is recommending *against* reclaiming this, which
    /// is a recommendation in its own right and not an absence of one.
    Keep,
}

impl Disposition {
    pub const ALL: [Disposition; 4] = [
        Disposition::RecommendNow,
        Disposition::AskUser,
        Disposition::Defer,
        Disposition::Keep,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::RecommendNow => "recommend_now",
            Self::AskUser => "ask_user",
            Self::Defer => "defer",
            Self::Keep => "keep",
        }
    }

    /// Exactly one of [`Self::ALL`]'s tags, or `None`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|d| d.tag() == tag)
    }
}

/// How the model says it knows a thing.
///
/// The three words campaign section 13 requires, and the one place in this
/// contract where the model is asked to grade its own claim. `Unknown` is not
/// a failure to answer — it is the answer, and the only honest one when a
/// probe came back unavailable.
///
/// What makes this load-bearing rather than decoration: the prompt tells the
/// model that absent evidence is `Unknown` and never a negative observation,
/// and a response that marks a claim `Observed` while citing an unavailable
/// fact is a response whose own confidence contradicts the projection it was
/// given. The claim is still only advisory — nothing here gates execution —
/// but a human reading the plan can see which sentences rest on something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimConfidence {
    /// Read directly off a fact the projection reported as observed.
    Observed,
    /// Reasoned from observed facts, and the reasoning could be wrong.
    Inferred,
    /// Not established. The value a missing, failed or not-attempted probe
    /// has to produce, because the alternative is a model reporting absence
    /// as a finding.
    Unknown,
}

impl ClaimConfidence {
    pub const ALL: [ClaimConfidence; 3] = [
        ClaimConfidence::Observed,
        ClaimConfidence::Inferred,
        ClaimConfidence::Unknown,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.tag() == tag)
    }
}

/// What one free-standing remark in a planner response is about.
///
/// Observations are the part of the response that is not attached to a
/// resource: "two of these worktrees disagree about whether the work landed",
/// "the recovery goal cannot be met from regenerable caches alone". They carry
/// no resource id, no action and no disposition, so there is nothing for them
/// to authorise — which is exactly why they are worth having, and why the
/// vocabulary is about the *shape* of the remark rather than about a subject.
///
/// Five kinds, each one a different reason a remark is worth a reader's
/// attention. There is deliberately no `general` or `other`: a remark that
/// fits none of these is one the product does not know how to present, and
/// [`super::validate`] drops it rather than showing an uncategorised sentence
/// from a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationKind {
    /// Two facts in the projection point in opposite directions.
    ConflictingEvidence,
    /// Something knowable was not established, and knowing it would change a
    /// recommendation. The kind campaign section 16's loop is built on:
    /// naming the gap is the step before asking for a probe.
    MissingEvidence,
    /// A remark about the layout of the workspace itself.
    WorkflowShape,
    /// A remark about how a resource came to exist or when it stops being
    /// worth keeping.
    ResourceLifecycle,
    /// A remark about whether the recovery goal is reachable from what is on
    /// the table. Never a verdict that it has been reached — deciding disk
    /// recovery is complete is not the model's to do.
    RecoveryOutlook,
}

impl ObservationKind {
    pub const ALL: [ObservationKind; 5] = [
        ObservationKind::ConflictingEvidence,
        ObservationKind::MissingEvidence,
        ObservationKind::WorkflowShape,
        ObservationKind::ResourceLifecycle,
        ObservationKind::RecoveryOutlook,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::ConflictingEvidence => "conflicting_evidence",
            Self::MissingEvidence => "missing_evidence",
            Self::WorkflowShape => "workflow_shape",
            Self::ResourceLifecycle => "resource_lifecycle",
            Self::RecoveryOutlook => "recovery_outlook",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.tag() == tag)
    }
}

/// The complete set of read-only probes a planner response may ask for.
///
/// # This is a registry, and it is closed at compile time
///
/// Campaign section 16 draws the line here: a model may say *which* question
/// it wants answered about *which* subject it was already shown, and nothing
/// else. It may not supply a program, an argument vector, a shell string, a
/// path, a URL or a credential — and the way that is guaranteed is not a
/// validation rule but the shape of this type. A `ProbeId` is one of seven
/// values. There is no variant that carries a payload, so there is nothing to
/// smuggle a command in; [`super::response::EvidenceRequestClaim`] is two
/// strings and a bounded question, both of which are looked up rather than
/// executed.
///
/// # What this ticket does and does not implement
///
/// HORO-1548 defines the vocabulary and makes an unknown probe id fail closed
/// at the parse boundary. Actually *running* a probe, bounding the rounds and
/// re-planning on the result is HORO-1549. Until then a validated request is
/// a recorded ask that reaches a report and nothing else — which is why the
/// vocabulary lands first: a closed set that already refuses unknown ids
/// cannot be widened accidentally by the ticket that gives it teeth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeId {
    /// Re-read one worktree's branch lifecycle: dirty, untracked, upstream,
    /// ahead/behind, containment.
    GitBranchState,
    /// Ask whether this worktree's commits have equivalents on the branch it
    /// was compared against — the cherry-pick and rebase question.
    GitPatchEquivalence,
    /// Ask again whether anything is using this worktree.
    ProcessActivity,
    /// Ask whether the tool that owns a resource is running.
    ToolLiveness,
    /// Ask the configured read-only GitHub provider about this branch's pull
    /// request.
    GithubPrState,
    /// Ask the configured read-only Jira provider about the task a branch
    /// names explicitly.
    JiraTaskState,
    /// Re-read the local workflow baseline.
    WorkspaceHistorySummary,
}

impl ProbeId {
    pub const ALL: [ProbeId; 7] = [
        ProbeId::GitBranchState,
        ProbeId::GitPatchEquivalence,
        ProbeId::ProcessActivity,
        ProbeId::ToolLiveness,
        ProbeId::GithubPrState,
        ProbeId::JiraTaskState,
        ProbeId::WorkspaceHistorySummary,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::GitBranchState => "git_branch_state",
            Self::GitPatchEquivalence => "git_patch_equivalence",
            Self::ProcessActivity => "process_activity",
            Self::ToolLiveness => "tool_liveness",
            Self::GithubPrState => "github_pr_state",
            Self::JiraTaskState => "jira_task_state",
            Self::WorkspaceHistorySummary => "workspace_history_summary",
        }
    }

    /// Exactly one of [`Self::ALL`]'s tags, or `None` — which
    /// [`super::validate`] turns into a dropped request rather than a probe
    /// nobody registered.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.tag() == tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tag round-trips, and no two variants of one vocabulary share a
    /// tag.
    ///
    /// The second half is the one worth having. A duplicated tag would make
    /// `from_tag` silently resolve to whichever variant `ALL` lists first,
    /// which is a bug no round-trip test can see — `tag(from_tag(t)) == t`
    /// holds for the survivor.
    #[test]
    fn tags_round_trip_and_are_distinct() {
        macro_rules! check {
            ($ty:ty) => {{
                let all = <$ty>::ALL;
                for value in all {
                    assert_eq!(
                        <$ty>::from_tag(value.tag()),
                        Some(value),
                        "{} did not round-trip through its own tag",
                        value.tag()
                    );
                }
                let mut tags: Vec<&str> = all.iter().map(|v| v.tag()).collect();
                let count = tags.len();
                tags.sort_unstable();
                tags.dedup();
                assert_eq!(tags.len(), count, "two variants share a tag: {tags:?}");
            }};
        }

        check!(Disposition);
        check!(ClaimConfidence);
        check!(ObservationKind);
        check!(ProbeId);
    }

    /// Each `ALL` lists every variant exactly once, in declaration order.
    ///
    /// The round-trip test above cannot see this. `from_tag` searches `ALL`, so
    /// a variant the author forgot to add there is silently unparseable — and
    /// `tags_round_trip_and_are_distinct` iterates `ALL` too, so it never asks
    /// about the missing one. Here the `match` is exhaustive, which makes the
    /// omission a build failure instead. The length assertions catch the
    /// reverse mistake: a duplicated or stale entry.
    ///
    /// Same idiom and same reasoning as
    /// `evidence::model::tests::all_lists_every_variant_exactly_once_in_declaration_order`.
    #[test]
    fn each_all_lists_every_variant_exactly_once() {
        for (index, value) in Disposition::ALL.iter().enumerate() {
            let expected = match value {
                Disposition::RecommendNow => 0,
                Disposition::AskUser => 1,
                Disposition::Defer => 2,
                Disposition::Keep => 3,
            };
            assert_eq!(index, expected, "{} is misplaced in ALL", value.tag());
        }
        assert_eq!(Disposition::ALL.len(), 4);

        for (index, value) in ClaimConfidence::ALL.iter().enumerate() {
            let expected = match value {
                ClaimConfidence::Observed => 0,
                ClaimConfidence::Inferred => 1,
                ClaimConfidence::Unknown => 2,
            };
            assert_eq!(index, expected, "{} is misplaced in ALL", value.tag());
        }
        assert_eq!(ClaimConfidence::ALL.len(), 3);

        for (index, value) in ObservationKind::ALL.iter().enumerate() {
            let expected = match value {
                ObservationKind::ConflictingEvidence => 0,
                ObservationKind::MissingEvidence => 1,
                ObservationKind::WorkflowShape => 2,
                ObservationKind::ResourceLifecycle => 3,
                ObservationKind::RecoveryOutlook => 4,
            };
            assert_eq!(index, expected, "{} is misplaced in ALL", value.tag());
        }
        assert_eq!(ObservationKind::ALL.len(), 5);

        for (index, value) in ProbeId::ALL.iter().enumerate() {
            let expected = match value {
                ProbeId::GitBranchState => 0,
                ProbeId::GitPatchEquivalence => 1,
                ProbeId::ProcessActivity => 2,
                ProbeId::ToolLiveness => 3,
                ProbeId::GithubPrState => 4,
                ProbeId::JiraTaskState => 5,
                ProbeId::WorkspaceHistorySummary => 6,
            };
            assert_eq!(index, expected, "{} is misplaced in ALL", value.tag());
        }
        assert_eq!(ProbeId::ALL.len(), 7);
    }

    /// Near misses are refused, not resolved.
    ///
    /// Each case is a string a model plausibly produces instead of the legal
    /// word: a truncation, a different separator, a different case, a
    /// synonym. All of them must be `None`. The dangerous direction is
    /// specific — a `"recommend"` that resolved to
    /// [`Disposition::RecommendNow`] would turn an ambiguous answer into a
    /// recommendation, and `"delete"` resolving to anything at all would be
    /// the model naming an outcome this vocabulary does not offer.
    #[test]
    fn near_misses_do_not_resolve() {
        for tag in [
            "recommend",
            "recommend-now",
            "RECOMMEND_NOW",
            "recommend_now ",
            "ask",
            "ask-user",
            "delete",
            "auto_safe",
            "",
        ] {
            assert_eq!(Disposition::from_tag(tag), None, "{tag:?} resolved");
        }

        for tag in ["high", "low", "certain", "Observed", "observed_", "guess"] {
            assert_eq!(ClaimConfidence::from_tag(tag), None, "{tag:?} resolved");
        }

        for tag in ["general", "other", "note", "conflict", "missing"] {
            assert_eq!(ObservationKind::from_tag(tag), None, "{tag:?} resolved");
        }

        for tag in [
            "git",
            "git_branch",
            "gitBranchState",
            "shell",
            "run_command",
            "rm -rf /",
        ] {
            assert_eq!(ProbeId::from_tag(tag), None, "{tag:?} resolved");
        }
    }

    /// No vocabulary offers a word that names an outcome the model is not
    /// allowed to choose.
    ///
    /// A guard against this module growing rather than against today's code:
    /// the way a closed vocabulary stops being a boundary is by gaining a
    /// convenient variant. `execute`, `delete`, `authorize`, `approve` and
    /// `protected` are the five that would matter — the first four because
    /// they name an act, the last because a model that could say `protected`
    /// would be naming a policy class.
    #[test]
    fn no_vocabulary_names_an_act_or_a_policy_class() {
        let every_tag: Vec<&str> = Disposition::ALL
            .iter()
            .map(|v| v.tag())
            .chain(ClaimConfidence::ALL.iter().map(|v| v.tag()))
            .chain(ObservationKind::ALL.iter().map(|v| v.tag()))
            .chain(ProbeId::ALL.iter().map(|v| v.tag()))
            .collect();

        for forbidden in [
            "execute",
            "delete",
            "remove",
            "authorize",
            "approve",
            "protected",
            "auto_safe",
            "ask",
        ] {
            assert!(
                !every_tag.contains(&forbidden),
                "a planner vocabulary now offers {forbidden:?}, which names something \
                 the model does not decide"
            );
        }
    }

    /// Every probe in the registry is read-only by name, and the registry is
    /// the seven campaign section 16 lists.
    ///
    /// Pinning the count is what makes the previous test non-vacuous: a
    /// widened registry fails here first, in a test whose message says why a
    /// new probe needs its own argument, rather than passing quietly because
    /// its tag happened to avoid the forbidden words.
    #[test]
    fn the_probe_registry_is_the_seven_read_only_questions() {
        assert_eq!(
            ProbeId::ALL.len(),
            7,
            "the probe registry changed size — a new probe must be justified as \
             read-only and bounded before it is offered to a provider"
        );
        for probe in ProbeId::ALL {
            let tag = probe.tag();
            for verb in ["write", "delete", "remove", "prune", "clean", "exec", "run"] {
                assert!(
                    !tag.contains(verb),
                    "probe {tag:?} names {verb:?} — the registry is read-only questions only"
                );
            }
        }
    }

    /// The version this build speaks is 2, and it is not 1.
    ///
    /// Trivial to the point of looking pointless, and it is here for one
    /// reason: the failure this contract exists to prevent is a v2 build
    /// reading a v1 response as though the missing fields meant something.
    /// A constant that silently went back to 1 would reintroduce exactly
    /// that, and nothing else in the suite pins it.
    #[test]
    fn the_contract_version_is_two() {
        assert_eq!(PLANNER_CONTRACT_VERSION, 2);
    }
}
