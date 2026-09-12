//! Evidence domain model (HORO-948).
//!
//! These types describe *what a detector observed about a resource*, never
//! *what should be done about it* — that judgment belongs to the policy
//! layer landing in a later ticket. Nothing here is `Deserialize`: these
//! are internal correlation/discovery types, not the LLM-facing shape a
//! future ticket will define separately.

use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use super::probe::ProbeOutcome;
use crate::detectors::DetectorId;

/// The kind of on-disk/tool-owned resource a detector found.
///
/// `Unknown` is a deliberate fail-closed sink: a resource that cannot be
/// classified into one of the known kinds carries a kind that the future
/// policy layer maps to `PROTECTED` unconditionally, rather than falling
/// through to some default treated as safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    XcodeDerivedData,
    HomebrewCache,
    CargoTargetDir,
    CargoRegistryCache,
    NodeModules,
    NodePackageManagerCache,
    DockerBuildCache,
    DockerImageCache,
    /// Fail-closed sink: a resource kind that cannot be classified. Policy
    /// (later ticket) maps this to PROTECTED unconditionally.
    Unknown,
}

impl ResourceKind {
    /// Tool that owns/manages this resource, for display and future policy
    /// use.
    pub fn owning_tool(&self) -> OwningTool {
        match self {
            ResourceKind::XcodeDerivedData => OwningTool::Xcode,
            ResourceKind::HomebrewCache => OwningTool::Homebrew,
            ResourceKind::CargoTargetDir | ResourceKind::CargoRegistryCache => OwningTool::Cargo,
            ResourceKind::NodeModules | ResourceKind::NodePackageManagerCache => OwningTool::Npm,
            ResourceKind::DockerBuildCache | ResourceKind::DockerImageCache => OwningTool::Docker,
            ResourceKind::Unknown => OwningTool::None,
        }
    }

    /// Static, per-kind regenerability. This is a property of the *kind*,
    /// not of any single observed instance.
    pub fn regenerability(&self) -> Regenerability {
        match self {
            ResourceKind::XcodeDerivedData => Regenerability::RegenerableByRebuild,
            ResourceKind::HomebrewCache => Regenerability::RegenerableByTool,
            ResourceKind::CargoTargetDir => Regenerability::RegenerableByRebuild,
            ResourceKind::CargoRegistryCache => Regenerability::RegenerableByTool,
            ResourceKind::NodeModules => Regenerability::RegenerableByRebuild,
            ResourceKind::NodePackageManagerCache => Regenerability::RegenerableByTool,
            ResourceKind::DockerBuildCache => Regenerability::RegenerableByTool,
            ResourceKind::DockerImageCache => Regenerability::RegenerableByTool,
            ResourceKind::Unknown => Regenerability::Unknown,
        }
    }

    /// Evidence fields required for an [`Evidence`] of this kind to be
    /// considered [`Completeness::Complete`]. This is the single source of
    /// truth [`Evidence::completeness`] checks against — extend this list,
    /// not the completeness logic, when a kind needs more evidence.
    ///
    /// `ToolLiveness` is only required for kinds whose owning tool has a
    /// real running-process signal to observe (Xcode.app, the Docker
    /// daemon). For `Cargo`/`Npm`/`Pnpm`/`Yarn`/`Homebrew`, `tool_liveness`
    /// is structurally always `Unavailable(ToolNotRunning)` — see
    /// [`crate::evidence::correlate::PgrepToolLivenessProbe`]
    /// — so requiring it here would make [`Completeness::Complete`]
    /// permanently unreachable for those kinds.
    pub fn required_evidence(&self) -> &'static [EvidenceField] {
        const WITHOUT_TOOL_LIVENESS: &[EvidenceField] = &[
            EvidenceField::LogicalBytes,
            EvidenceField::ReclaimableBytes,
            EvidenceField::LastModified,
            EvidenceField::OpenByProcess,
            EvidenceField::ProcessCwdMatch,
            EvidenceField::GitState,
        ];
        const WITH_TOOL_LIVENESS: &[EvidenceField] = &[
            EvidenceField::LogicalBytes,
            EvidenceField::ReclaimableBytes,
            EvidenceField::LastModified,
            EvidenceField::OpenByProcess,
            EvidenceField::ProcessCwdMatch,
            EvidenceField::GitState,
            EvidenceField::ToolLiveness,
        ];
        match self {
            ResourceKind::XcodeDerivedData
            | ResourceKind::DockerBuildCache
            | ResourceKind::DockerImageCache => WITH_TOOL_LIVENESS,
            ResourceKind::HomebrewCache
            | ResourceKind::CargoTargetDir
            | ResourceKind::CargoRegistryCache
            | ResourceKind::NodeModules
            | ResourceKind::NodePackageManagerCache
            | ResourceKind::Unknown => WITHOUT_TOOL_LIVENESS,
        }
    }
}

/// Tool that owns/manages a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwningTool {
    Xcode,
    Homebrew,
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Docker,
    None,
}

impl fmt::Display for OwningTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            OwningTool::Xcode => "xcode",
            OwningTool::Homebrew => "homebrew",
            OwningTool::Cargo => "cargo",
            OwningTool::Npm => "npm",
            OwningTool::Pnpm => "pnpm",
            OwningTool::Yarn => "yarn",
            OwningTool::Docker => "docker",
            OwningTool::None => "none",
        };
        f.write_str(s)
    }
}

/// How to locate a resource: a filesystem path, or a tool-native id (e.g.
/// a Docker build cache identifier) that has no single canonical path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceLocator {
    /// A filesystem path. MUST always be canonicalized before being stored
    /// here — never store a raw, un-canonicalized path.
    Path(PathBuf),
    /// A tool-native identifier with no single canonical filesystem path.
    Tool { tool: OwningTool, id: String },
}

impl fmt::Display for ResourceLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceLocator::Path(p) => write!(f, "{}", p.display()),
            ResourceLocator::Tool { tool, id } => write!(f, "{tool}:{id}"),
        }
    }
}

/// Stable identity of one resource. [`ResourceId`]'s [`Display`](std::fmt::Display) output is
/// the ONLY resource handle a future LLM will ever see — keep the format
/// stable: `"<resource_kind_snake_case>:<locator>"`, e.g.
/// `"cargo_target_dir:/Users/x/proj/target"`.
#[derive(Debug, Clone)]
pub struct ResourceId {
    pub kind: ResourceKind,
    pub locator: ResourceLocator,
    /// The project root a detector actually discovered this resource
    /// under, BEFORE any symlink resolution — e.g. the entry from
    /// [`crate::detectors::DiscoveryContext::known_project_roots`] that
    /// [`crate::detectors::cargo::CargoDetector`] scanned to find this
    /// `target/` directory.
    ///
    /// This exists to fix HORO-1017: when a project's `target/` is a
    /// symlink to a physically different location, deriving the Cargo
    /// manifest path from `locator`'s canonicalized (symlink-resolved)
    /// path looks for `Cargo.toml` next to the *physical* target
    /// location, not next to the project root the detector actually
    /// discovered the resource under — and an unrelated project's
    /// `Cargo.toml` sitting next to that physical location would be
    /// picked up instead. `source_project_root` carries the real,
    /// pre-canonicalization root forward so actions can bind their
    /// manifest lookup to it instead of re-deriving identity from
    /// `locator`.
    ///
    /// `None` for every resource kind that has no such concept (Xcode,
    /// Homebrew, Node, Docker) — only the Cargo detector ever populates
    /// `Some(_)`.
    pub source_project_root: Option<PathBuf>,
}

/// Identity is `kind` + `locator` only. `source_project_root` is
/// provenance metadata (see its own doc comment above), not part of
/// resource identity — two `ResourceId`s discovered via different raw
/// `--project-root` spellings of the same physical directory must still
/// be recognized as the same resource by any `HashSet<ResourceId>` /
/// `HashMap<ResourceId, _>` (HORO-1019).
impl PartialEq for ResourceId {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.locator == other.locator
    }
}

impl Eq for ResourceId {}

impl std::hash::Hash for ResourceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.locator.hash(state);
    }
}

impl ResourceId {
    pub fn new(kind: ResourceKind, locator: ResourceLocator) -> Self {
        Self {
            kind,
            locator,
            source_project_root: None,
        }
    }

    /// Attach the project root this resource was actually discovered
    /// under. See the field doc comment on [`ResourceId::source_project_root`]
    /// for why this exists.
    pub fn with_source_project_root(mut self, root: PathBuf) -> Self {
        self.source_project_root = Some(root);
        self
    }

    /// Stable, snake_case tag for `self.kind` — the same string
    /// [`ResourceId`]'s [`Display`](std::fmt::Display) impl uses before the locator. `pub`
    /// (HORO-954) so `actions::llm::LlmResourceView` can reuse this exact
    /// mapping instead of re-deriving its own copy of this match.
    pub fn kind_tag(&self) -> &'static str {
        match self.kind {
            ResourceKind::XcodeDerivedData => "xcode_derived_data",
            ResourceKind::HomebrewCache => "homebrew_cache",
            ResourceKind::CargoTargetDir => "cargo_target_dir",
            ResourceKind::CargoRegistryCache => "cargo_registry_cache",
            ResourceKind::NodeModules => "node_modules",
            ResourceKind::NodePackageManagerCache => "node_package_manager_cache",
            ResourceKind::DockerBuildCache => "docker_build_cache",
            ResourceKind::DockerImageCache => "docker_image_cache",
            ResourceKind::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind_tag(), self.locator)
    }
}

/// Cheap identity fingerprint used to detect whether a resource observed
/// earlier is "the same" resource later (device+inode, mtime, and/or a
/// tool-reported revision string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFingerprint {
    pub dev_ino: Option<(u64, u64)>,
    pub mtime: Option<SystemTime>,
    pub tool_revision: Option<String>,
}

/// Static, per-kind property: can this resource be regenerated, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regenerability {
    RegenerableByTool,
    RegenerableByRebuild,
    NotRegenerable,
    Unknown,
}

/// Per-instance judgment of what happens if this specific resource is
/// removed. Distinct from [`Regenerability`] (a static per-kind property):
/// `Recoverability` is what a later ticket's policy will actually reason
/// over for a given observed [`Evidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recoverability {
    RegenerableByTool,
    RegenerableByRebuild,
    Irreversible,
}

/// A stable identifier for a pre-registered, typed cleanup action. Defined
/// here (not in an `actions` module, which doesn't exist yet) because
/// [`NativeCleanup`] needs it now; the future `actions` module will reuse
/// this same type rather than defining its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(pub &'static str);

/// Whether a detector knows of a safe, native cleanup command for this
/// resource kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCleanup {
    Available(ActionId),
    Unsupported,
}

/// One discrete field of evidence that [`ResourceKind::required_evidence`]
/// can require and [`Evidence::completeness`] checks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceField {
    LogicalBytes,
    ReclaimableBytes,
    LastModified,
    OpenByProcess,
    ProcessCwdMatch,
    GitState,
    ToolLiveness,
}

/// A single process discovered (via [`crate::evidence::correlate`]) to
/// have a resource open, or to have it as its current working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRef {
    pub pid: u32,
    pub command: String,
}

/// Git repository state for a resource's containing directory, as
/// determined by [`crate::evidence::correlate::GitProbe`].
///
/// This type only exists when `path` is genuinely inside a git working
/// tree — see the doc comment on [`Evidence::git_state`] for why the
/// field wraps this in `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    pub repo_root: PathBuf,
    pub dirty: bool,
    pub untracked: bool,
    pub worktree: bool,
}

/// Bound on how many provenance notes an [`Evidence`] retains in
/// `sources`.
const MAX_SOURCES: usize = 8;

/// Everything one detector observed about one resource at one point in
/// time.
///
/// This ticket (HORO-948) only ever populates the discovery-stage probe
/// fields (`logical_bytes`, `reclaimable_bytes`, `last_modified`,
/// `last_accessed`) plus the static `regenerability`/`recoverability`/
/// `native_cleanup` judgments. The four correlation fields
/// (`open_by_process`, `process_cwd_match`, `git_state`, `tool_liveness`)
/// are ALWAYS `ProbeOutcome::Unavailable(ProbeReason::NotAttempted)` here —
/// [`crate::evidence::correlate`] (HORO-949) is responsible for actually
/// attempting that correlation and filling these in via
/// [`crate::evidence::correlate::merge_into`]. This is what keeps a
/// freshly-discovered `Evidence` from ever reporting
/// [`Completeness::Complete`] before correlation runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub resource: ResourceId,
    pub fingerprint: ResourceFingerprint,
    pub detector: DetectorId,
    pub logical_bytes: ProbeOutcome<u64>,
    /// Physical (on-disk block) size. MVP always leaves this `None` —
    /// subtree `st_blocks` summation is explicitly out of scope for this
    /// ticket.
    pub physical_bytes: Option<u64>,
    pub reclaimable_bytes: ProbeOutcome<u64>,
    pub last_modified: ProbeOutcome<SystemTime>,
    pub last_accessed: ProbeOutcome<SystemTime>,
    pub regenerability: Regenerability,
    pub recoverability: Recoverability,
    pub native_cleanup: NativeCleanup,
    // Correlation fields — HORO-948 always sets these to `NotAttempted`;
    // `crate::evidence::correlate` (HORO-949) fills them in.
    pub open_by_process: ProbeOutcome<Vec<ProcessRef>>,
    pub process_cwd_match: ProbeOutcome<Vec<ProcessRef>>,
    /// `Observed(None)` means the probe ran successfully and determined
    /// the resource is genuinely not inside a git working tree — a
    /// legitimate, complete answer, not a missing one. Only
    /// `Unavailable(reason)` means the probe itself failed. See
    /// [`crate::evidence::correlate::GitProbe`].
    pub git_state: ProbeOutcome<Option<GitState>>,
    pub tool_liveness: ProbeOutcome<bool>,
    pub collected_at: SystemTime,
    /// Bounded provenance notes (capped at `MAX_SOURCES` entries).
    pub sources: Vec<String>,
}

impl Evidence {
    /// Push a provenance note, silently dropping it once `sources` has
    /// reached `MAX_SOURCES` — provenance is advisory context, not a
    /// field callers should rely on being exhaustive.
    pub fn push_source(&mut self, note: impl Into<String>) {
        if self.sources.len() < MAX_SOURCES {
            self.sources.push(note.into());
        }
    }

    /// Derived, never stored: compares each relevant field's
    /// [`ProbeOutcome`] against [`ResourceKind::required_evidence`] for
    /// this resource's kind. Any required field that is `Unavailable` is
    /// "missing".
    pub fn completeness(&self) -> Completeness {
        let mut missing = Vec::new();
        for field in self.resource.kind.required_evidence() {
            let observed = match field {
                EvidenceField::LogicalBytes => self.logical_bytes.is_observed(),
                EvidenceField::ReclaimableBytes => self.reclaimable_bytes.is_observed(),
                EvidenceField::LastModified => self.last_modified.is_observed(),
                EvidenceField::OpenByProcess => self.open_by_process.is_observed(),
                EvidenceField::ProcessCwdMatch => self.process_cwd_match.is_observed(),
                EvidenceField::GitState => self.git_state.is_observed(),
                EvidenceField::ToolLiveness => self.tool_liveness.is_observed(),
            };
            if !observed {
                missing.push(*field);
            }
        }

        if missing.is_empty() {
            Completeness::Complete
        } else if missing.len() == self.resource.kind.required_evidence().len() {
            Completeness::Failed
        } else {
            Completeness::Partial { missing }
        }
    }

    /// Advisory-only confidence, derived from [`Evidence::completeness`].
    /// NOT consumed by the policy layer (future ticket) — this exists to
    /// give a human/LLM explanation something short to point at.
    pub fn confidence(&self) -> Confidence {
        match self.completeness() {
            Completeness::Complete => Confidence::High,
            Completeness::Partial { missing } if missing.len() <= 1 => Confidence::Medium,
            _ => Confidence::Low,
        }
    }
}

/// Derived completeness of one [`Evidence`] record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Partial { missing: Vec<EvidenceField> },
    Failed,
}

/// Advisory confidence level derived from [`Completeness`]. Explain-text
/// only, never consumed by policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::probe::ProbeReason;

    fn base_evidence(kind: ResourceKind) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from("/tmp/x"))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            last_modified: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::Unknown,
            recoverability: Recoverability::Irreversible,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn resource_id_display_is_stable_kind_colon_locator() {
        let id = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/proj/target")),
        );
        assert_eq!(id.to_string(), "cargo_target_dir:/Users/x/proj/target");
    }

    #[test]
    fn resource_id_display_for_tool_locator() {
        let id = ResourceId::new(
            ResourceKind::DockerBuildCache,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: "abc123".to_string(),
            },
        );
        assert_eq!(id.to_string(), "docker_build_cache:docker:abc123");
    }

    #[test]
    fn resource_id_equality_and_hash_ignore_source_project_root() {
        // HORO-1019 regression: two different raw `--project-root`
        // spellings of the same physical directory must not defeat
        // dedup in a HashSet<ResourceId>/HashMap<ResourceId, _> —
        // source_project_root is provenance metadata, not identity.
        let id_a = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/tmp/proj/target")),
        )
        .with_source_project_root(PathBuf::from("/tmp/proj"));
        let id_b = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/tmp/proj/target")),
        )
        .with_source_project_root(PathBuf::from("/tmp/proj/../proj"));

        assert_ne!(
            id_a.source_project_root, id_b.source_project_root,
            "test fixture must exercise genuinely different source_project_root strings"
        );
        assert_eq!(id_a, id_b);

        let mut set = std::collections::HashSet::new();
        set.insert(id_a);
        set.insert(id_b);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn discovery_stage_evidence_with_no_correlation_is_never_complete() {
        // Discovery stage: only logical_bytes observed, everything else
        // (including all four correlation fields) is NotAttempted.
        let mut evidence = base_evidence(ResourceKind::CargoTargetDir);
        evidence.logical_bytes = ProbeOutcome::Observed(1024);

        match evidence.completeness() {
            Completeness::Complete => panic!("discovery-stage evidence must never be Complete"),
            Completeness::Partial { missing } => {
                assert!(missing.contains(&EvidenceField::OpenByProcess));
                assert!(missing.contains(&EvidenceField::GitState));
            }
            Completeness::Failed => {}
        }
        assert_eq!(evidence.confidence(), Confidence::Low);
    }

    #[test]
    fn fully_observed_evidence_is_complete_and_high_confidence() {
        let mut evidence = base_evidence(ResourceKind::CargoTargetDir);
        evidence.logical_bytes = ProbeOutcome::Observed(1024);
        evidence.reclaimable_bytes = ProbeOutcome::Observed(1024);
        evidence.last_modified = ProbeOutcome::Observed(SystemTime::UNIX_EPOCH);
        evidence.open_by_process = ProbeOutcome::Observed(Vec::new());
        evidence.process_cwd_match = ProbeOutcome::Observed(Vec::new());
        // `Observed(None)` — "determined this isn't a git repo" — is a
        // fully-observed answer, not a missing one.
        evidence.git_state = ProbeOutcome::Observed(None);
        evidence.tool_liveness = ProbeOutcome::Observed(true);

        assert_eq!(evidence.completeness(), Completeness::Complete);
        assert_eq!(evidence.confidence(), Confidence::High);
    }

    #[test]
    fn non_daemon_tool_kinds_reach_complete_despite_tool_liveness_unavailable() {
        // Cargo/Node/Homebrew have no persistent daemon, so tool_liveness is
        // structurally always Unavailable(ToolNotRunning) for them (see
        // crate::evidence::correlate::tool_liveness). required_evidence()
        // must exclude ToolLiveness for these kinds so Complete stays
        // reachable — otherwise AUTO_SAFE would be unreachable for them.
        for kind in [
            ResourceKind::CargoTargetDir,
            ResourceKind::CargoRegistryCache,
            ResourceKind::NodeModules,
            ResourceKind::NodePackageManagerCache,
            ResourceKind::HomebrewCache,
        ] {
            let mut evidence = base_evidence(kind);
            evidence.logical_bytes = ProbeOutcome::Observed(1024);
            evidence.reclaimable_bytes = ProbeOutcome::Observed(1024);
            evidence.last_modified = ProbeOutcome::Observed(SystemTime::UNIX_EPOCH);
            evidence.open_by_process = ProbeOutcome::Observed(Vec::new());
            evidence.process_cwd_match = ProbeOutcome::Observed(Vec::new());
            evidence.git_state = ProbeOutcome::Observed(None);
            evidence.tool_liveness = ProbeOutcome::Unavailable(ProbeReason::ToolNotRunning);

            assert_eq!(
                evidence.completeness(),
                Completeness::Complete,
                "{kind:?} should reach Complete with tool_liveness Unavailable(ToolNotRunning)"
            );
        }
    }

    #[test]
    fn evidence_with_nothing_observed_is_failed_completeness() {
        let evidence = base_evidence(ResourceKind::CargoTargetDir);
        assert_eq!(evidence.completeness(), Completeness::Failed);
        assert_eq!(evidence.confidence(), Confidence::Low);
    }

    #[test]
    fn push_source_is_capped_at_max_sources() {
        let mut evidence = base_evidence(ResourceKind::CargoTargetDir);
        for i in 0..20 {
            evidence.push_source(format!("note-{i}"));
        }
        assert_eq!(evidence.sources.len(), MAX_SOURCES);
    }

    #[test]
    fn unknown_kind_regenerability_is_unknown() {
        assert_eq!(
            ResourceKind::Unknown.regenerability(),
            Regenerability::Unknown
        );
        assert_eq!(ResourceKind::Unknown.owning_tool(), OwningTool::None);
    }
}
