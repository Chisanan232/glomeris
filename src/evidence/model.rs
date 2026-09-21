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

    /// Stable, snake_case tag for this kind — the single source of truth
    /// [`ResourceId::kind_tag`] delegates to, and that
    /// `actions list --json` (HORO-1047) also uses directly to project an
    /// [`crate::actions::Action::applies_to`] slice (which carries bare
    /// [`ResourceKind`] values, not a full [`ResourceId`]).
    pub fn tag(&self) -> &'static str {
        match self {
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

    /// Every kind, in declaration order. Exhaustive by construction: the
    /// `every_variant_round_trips_through_its_tag` test below fails if a
    /// new variant is added to the enum without being added here, because
    /// [`from_tag`](Self::from_tag)'s own match arms and this list are
    /// checked against each other.
    pub const ALL: &'static [ResourceKind] = &[
        ResourceKind::XcodeDerivedData,
        ResourceKind::HomebrewCache,
        ResourceKind::CargoTargetDir,
        ResourceKind::CargoRegistryCache,
        ResourceKind::NodeModules,
        ResourceKind::NodePackageManagerCache,
        ResourceKind::DockerBuildCache,
        ResourceKind::DockerImageCache,
        ResourceKind::Unknown,
    ];

    /// Parses a [`tag`](Self::tag) back into its kind, or `None` for any
    /// string that is not exactly one of them (HORO-1310).
    ///
    /// Deliberately NOT lenient: no case folding, no trimming, no
    /// hyphen/underscore equivalence, no prefix matching. This is the
    /// parser an Autopilot resource-kind allowlist is built through — both
    /// from a CLI flag and from a persisted envelope file — so a typo must
    /// fail loudly rather than resolve to a neighbouring kind. An
    /// unrecognized tag naming a kind that does not exist is a smaller
    /// problem than `cargo_target` silently meaning
    /// [`ResourceKind::CargoTargetDir`].
    ///
    /// `"unknown"` does round-trip here — this function's job is parsing,
    /// not authorization. [`ResourceKind::Unknown`] is refused where it
    /// matters (it is unconditionally `PROTECTED` in
    /// [`crate::policy::classify`], and
    /// `crate::autopilot::AutopilotEnvelope` refuses to allowlist it at
    /// all), and a tag that parses to it is far better than one that
    /// silently parses to nothing.
    pub fn from_tag(tag: &str) -> Option<ResourceKind> {
        ResourceKind::ALL.iter().copied().find(|k| k.tag() == tag)
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
/// the resource handle every CLI report, `--resource-id` selector and
/// hand-written `--plan-file` fixture uses — keep the format stable:
/// `"<resource_kind_snake_case>:<locator>"`, e.g.
/// `"cargo_target_dir:/Users/x/proj/target"`.
///
/// It is NOT what an LLM sees. As the example above shows, this string
/// embeds an absolute path for every path-backed resource kind, so
/// `crate::actions::llm` substitutes a positional wire alias before the
/// request leaves the machine (HORO-1298). Local use of this format is
/// unrestricted; adding an egress path for it is not.
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
    /// mapping instead of re-deriving its own copy of this match. Delegates
    /// to [`ResourceKind::tag`], the single source of truth.
    pub fn kind_tag(&self) -> &'static str {
        self.kind.tag()
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

const FP_DEV_INO_BIT: u8 = 0b001;
const FP_MTIME_BIT: u8 = 0b010;
const FP_TOOL_REVISION_BIT: u8 = 0b100;

/// Error returned by [`decode_fingerprint_token`] when a token is
/// malformed — truncated, non-hex, an unsupported version prefix, or
/// otherwise not something [`encode_fingerprint_token`] could have
/// produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintTokenError(String);

impl fmt::Display for FingerprintTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid fingerprint token: {}", self.0)
    }
}

impl std::error::Error for FingerprintTokenError {}

/// Encodes a [`ResourceFingerprint`] as an opaque, wire-safe string
/// (HORO-1051).
///
/// This exists so a future interactive `execute` subcommand (HORO-1055)
/// can carry the exact fingerprint the UI observed at `explain` time
/// across process boundaries to a later `execute` invocation, so
/// [`crate::policy::approval::authorize`] can compare it against the
/// freshly-observed fingerprint at execution time — reusing the existing
/// fingerprint-pinning invariant `authorize` already enforces via
/// `PartialEq`, not inventing a new consent mechanism. The only contract
/// callers may rely on is: `decode_fingerprint_token(&encode_fingerprint_token(fp))
/// == Ok(fp)` for every `fp`. The string layout is deliberately
/// undocumented outside this pair of functions and must be treated as
/// opaque.
///
/// Internally: `"v1:"` followed by a hex-encoded, hand-rolled binary
/// layout — a 1-byte presence bitmask, then each present field in
/// `dev_ino`, `mtime`, `tool_revision` order. `mtime` is stored as a sign
/// byte plus 8-byte seconds and 4-byte nanoseconds (big-endian) relative
/// to `UNIX_EPOCH`, which preserves full `SystemTime` precision — no
/// rounding to whole seconds.
pub fn encode_fingerprint_token(fingerprint: &ResourceFingerprint) -> String {
    let mut flags = 0u8;
    if fingerprint.dev_ino.is_some() {
        flags |= FP_DEV_INO_BIT;
    }
    if fingerprint.mtime.is_some() {
        flags |= FP_MTIME_BIT;
    }
    if fingerprint.tool_revision.is_some() {
        flags |= FP_TOOL_REVISION_BIT;
    }

    let mut bytes = vec![flags];

    if let Some((dev, ino)) = fingerprint.dev_ino {
        bytes.extend_from_slice(&dev.to_be_bytes());
        bytes.extend_from_slice(&ino.to_be_bytes());
    }

    if let Some(mtime) = fingerprint.mtime {
        let (sign, duration) = match mtime.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => (1u8, d),
            Err(e) => (0u8, e.duration()),
        };
        bytes.push(sign);
        bytes.extend_from_slice(&duration.as_secs().to_be_bytes());
        bytes.extend_from_slice(&duration.subsec_nanos().to_be_bytes());
    }

    if let Some(revision) = &fingerprint.tool_revision {
        let raw = revision.as_bytes();
        bytes.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        bytes.extend_from_slice(raw);
    }

    let mut token = String::with_capacity(bytes.len() * 2 + 3);
    token.push_str("v1:");
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// Reads exactly `n` bytes starting at `*cursor`, advancing `*cursor` past
/// them. Shared truncation/overflow check for every field
/// [`decode_fingerprint_token`] pulls off the wire.
fn take_bytes<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    n: usize,
) -> Result<&'a [u8], FingerprintTokenError> {
    let end = cursor
        .checked_add(n)
        .ok_or_else(|| FingerprintTokenError("length overflow".to_string()))?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| FingerprintTokenError("truncated payload".to_string()))?;
    *cursor = end;
    Ok(slice)
}

/// Decodes a token produced by [`encode_fingerprint_token`] back into a
/// [`ResourceFingerprint`]. See that function's doc comment for the
/// round-trip contract this must uphold.
pub fn decode_fingerprint_token(token: &str) -> Result<ResourceFingerprint, FingerprintTokenError> {
    let hex = token.strip_prefix("v1:").ok_or_else(|| {
        FingerprintTokenError("unsupported or missing version prefix".to_string())
    })?;

    if hex.len() % 2 != 0 {
        return Err(FingerprintTokenError("odd-length hex payload".to_string()));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let s = std::str::from_utf8(chunk)
            .map_err(|_| FingerprintTokenError("non-UTF8 hex payload".to_string()))?;
        let byte = u8::from_str_radix(s, 16)
            .map_err(|_| FingerprintTokenError("invalid hex byte".to_string()))?;
        bytes.push(byte);
    }

    let mut cursor = 0usize;
    let flags = *take_bytes(&bytes, &mut cursor, 1)?.first().unwrap();

    let dev_ino = if flags & FP_DEV_INO_BIT != 0 {
        let dev = u64::from_be_bytes(take_bytes(&bytes, &mut cursor, 8)?.try_into().unwrap());
        let ino = u64::from_be_bytes(take_bytes(&bytes, &mut cursor, 8)?.try_into().unwrap());
        Some((dev, ino))
    } else {
        None
    };

    let mtime = if flags & FP_MTIME_BIT != 0 {
        let sign = *take_bytes(&bytes, &mut cursor, 1)?.first().unwrap();
        let secs = u64::from_be_bytes(take_bytes(&bytes, &mut cursor, 8)?.try_into().unwrap());
        let nanos = u32::from_be_bytes(take_bytes(&bytes, &mut cursor, 4)?.try_into().unwrap());
        let duration = std::time::Duration::new(secs, nanos);
        let t = if sign == 1 {
            SystemTime::UNIX_EPOCH.checked_add(duration)
        } else {
            SystemTime::UNIX_EPOCH.checked_sub(duration)
        };
        Some(t.ok_or_else(|| FingerprintTokenError("mtime out of range".to_string()))?)
    } else {
        None
    };

    let tool_revision = if flags & FP_TOOL_REVISION_BIT != 0 {
        let len =
            u32::from_be_bytes(take_bytes(&bytes, &mut cursor, 4)?.try_into().unwrap()) as usize;
        let raw = take_bytes(&bytes, &mut cursor, len)?;
        Some(
            String::from_utf8(raw.to_vec())
                .map_err(|_| FingerprintTokenError("invalid utf8 tool_revision".to_string()))?,
        )
    } else {
        None
    };

    if cursor != bytes.len() {
        return Err(FingerprintTokenError(
            "trailing bytes after payload".to_string(),
        ));
    }

    Ok(ResourceFingerprint {
        dev_ino,
        mtime,
        tool_revision,
    })
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

/// What each field *is*, in the words a user would use for it.
///
/// Needed because `ActionError::MissingRequiredEvidence` is a user-facing
/// refusal — it reaches a terminal via `clean --dry-run` and a row in the
/// menu-bar app's AI Plan card — and "OpenByProcess" describes the variant
/// rather than the missing measurement.
impl fmt::Display for EvidenceField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let described = match self {
            Self::LogicalBytes => "how big it is",
            Self::ReclaimableBytes => "how much space removing it would free",
            Self::LastModified => "when it was last changed",
            Self::OpenByProcess => "whether a running process has it open",
            Self::ProcessCwdMatch => "whether a running process is working inside it",
            Self::GitState => "the state of its git repository",
            Self::ToolLiveness => "whether the tool that owns it is running",
        };
        write!(f, "{described}")
    }
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
    /// `true` when `reclaimable_bytes` (and `logical_bytes`, which shares
    /// the same estimate for every detector that sets this) is a truthful
    /// lower bound rather than a settled measurement — i.e. the
    /// size-estimate walk that produced it stopped early on
    /// [`crate::detectors::SizeEstimate`]'s own entry/time budget
    /// (HORO-1016) rather than exhausting the subtree. Set directly from
    /// that typed signal at discovery/revalidation time — never derived by
    /// string-sniffing `sources`/`SizeEstimate::lower_bound_note`. Purely a
    /// display/reporting concern: `policy::classify()` must never read
    /// this field (see the `classify()`-invariance regression test in
    /// `src/policy/engine.rs`'s test module).
    pub reclaimable_bytes_is_lower_bound: bool,
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

    /// Compile-time guard in both directions. The `match` is exhaustive, so
    /// adding a variant to [`ResourceKind`] without adding it to
    /// [`ResourceKind::ALL`] fails to build here — which matters because
    /// [`ResourceKind::from_tag`] searches `ALL`, so a missing entry would
    /// silently make the new kind unparseable (and therefore impossible to
    /// name in an Autopilot allowlist) rather than loudly wrong. The length
    /// assertion catches the opposite mistake: an entry in `ALL` for a
    /// variant that no longer exists, or a duplicate.
    #[test]
    fn all_lists_every_variant_exactly_once_in_declaration_order() {
        for (index, kind) in ResourceKind::ALL.iter().enumerate() {
            let expected_index = match kind {
                ResourceKind::XcodeDerivedData => 0,
                ResourceKind::HomebrewCache => 1,
                ResourceKind::CargoTargetDir => 2,
                ResourceKind::CargoRegistryCache => 3,
                ResourceKind::NodeModules => 4,
                ResourceKind::NodePackageManagerCache => 5,
                ResourceKind::DockerBuildCache => 6,
                ResourceKind::DockerImageCache => 7,
                ResourceKind::Unknown => 8,
            };
            assert_eq!(
                index,
                expected_index,
                "{} is at index {index} of ResourceKind::ALL, expected {expected_index}",
                kind.tag()
            );
        }
        assert_eq!(
            ResourceKind::ALL.len(),
            9,
            "ResourceKind::ALL has gained, lost, or duplicated an entry"
        );
    }

    #[test]
    fn every_variant_round_trips_through_its_tag() {
        for kind in ResourceKind::ALL {
            assert_eq!(
                ResourceKind::from_tag(kind.tag()),
                Some(*kind),
                "tag {:?} did not round-trip",
                kind.tag()
            );
        }
    }

    /// `from_tag` is the parser an Autopilot allowlist is built through, so
    /// near-misses must fail rather than resolve to a neighbouring kind.
    #[test]
    fn from_tag_rejects_near_misses_rather_than_guessing() {
        for near_miss in [
            "",
            " cargo_target_dir",
            "cargo_target_dir ",
            "cargo_target",
            "cargo-target-dir",
            "CARGO_TARGET_DIR",
            "CargoTargetDir",
            "cargo_target_dirs",
            "not_a_kind",
        ] {
            assert_eq!(
                ResourceKind::from_tag(near_miss),
                None,
                "{near_miss:?} must not parse as a resource kind"
            );
        }
    }

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
            reclaimable_bytes_is_lower_bound: false,
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

/// Tests for the opaque fingerprint token codec (HORO-1051). Kept as its
/// own module so the round-trip/`authorize` interaction tests — which pull
/// in `crate::policy` and touch real files on disk — stay separate from
/// `mod tests` above's pure in-memory `Evidence` fixtures.
#[cfg(test)]
mod fingerprint_token_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::policy::approval::authorize;
    use crate::policy::{PolicyClass, PolicyDecision, ReasonCode, UserConsent};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-fp-token-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Builds a [`ResourceFingerprint`] from a REAL file on disk (never a
    /// hand-built/fabricated value), with its mtime explicitly set to a
    /// timestamp carrying sub-second precision — a codec that silently
    /// rounds `mtime` to whole seconds must fail every test that uses this
    /// helper with a non-zero `nanos`.
    fn real_file_fingerprint(dir: &std::path::Path, name: &str, nanos: u32) -> ResourceFingerprint {
        let path = dir.join(name);
        fs::write(&path, b"hello fingerprint token").expect("write temp file");

        let mtime = SystemTime::UNIX_EPOCH + Duration::new(1_725_000_000, nanos);
        let file = fs::File::open(&path).expect("open temp file");
        file.set_modified(mtime).expect("set mtime");

        // Use the SAME producers real evidence-collection uses
        // (`discovery_evidence` in `src/detectors/mod.rs`) rather than
        // re-deriving dev/ino/mtime by hand, so this test proves the codec
        // round-trips what production actually emits. `tool_revision` has
        // no real-file source, so it's the one deliberately fabricated
        // field — included to exercise that branch of the codec.
        ResourceFingerprint {
            dev_ino: crate::detectors::dev_ino_fingerprint(&path),
            mtime: crate::detectors::probe_mtime(&path).observed().copied(),
            tool_revision: Some("v1.2.3-real".to_string()),
        }
    }

    /// AC 1: round-trip over a fingerprint derived from a real temp-dir
    /// file, precise enough (explicit sub-second mtime) to expose a lossy
    /// encoding.
    #[test]
    fn round_trip_preserves_real_file_fingerprint_exactly() {
        let dir = unique_temp_dir("roundtrip");
        let fp = real_file_fingerprint(&dir, "resource.bin", 123_456_789);
        assert_ne!(
            fp.mtime
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos(),
            0,
            "test fixture must carry real sub-second mtime precision"
        );

        let token = encode_fingerprint_token(&fp);
        let decoded = decode_fingerprint_token(&token).expect("token should decode");

        assert_eq!(
            decoded, fp,
            "decoded fingerprint must equal the original real-file fingerprint \
             bit-for-bit, including sub-second mtime precision"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// AC 2: decoding then re-encoding a token must reproduce a
    /// byte-identical token to encoding the original fingerprint directly.
    #[test]
    fn decode_then_reencode_matches_direct_encode() {
        let dir = unique_temp_dir("stability");
        let fp = real_file_fingerprint(&dir, "resource.bin", 555_555_555);

        let token = encode_fingerprint_token(&fp);
        let decoded = decode_fingerprint_token(&token).expect("token should decode");
        let re_encoded = encode_fingerprint_token(&decoded);

        assert_eq!(
            re_encoded, token,
            "re-encoding a decoded fingerprint must reproduce the original token exactly"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// AC 3 — the property that actually matters: `authorize()` must
    /// behave identically whether given the original fingerprint or one
    /// that went through encode -> decode, both when it matches consent
    /// (authorizes) and when it doesn't (refuses). A token that is merely
    /// "structurally similar" but not a true substitute would fail this.
    #[test]
    fn authorize_treats_decoded_fingerprint_as_real_substitute() {
        let dir = unique_temp_dir("authorize");
        let fp = real_file_fingerprint(&dir, "resource.bin", 987_654_321);
        let other_fp = real_file_fingerprint(&dir, "other.bin", 111);

        let token = encode_fingerprint_token(&fp);
        let decoded = decode_fingerprint_token(&token).expect("token should decode");
        assert_eq!(decoded, fp);

        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(dir.join("resource.bin")),
        );
        let consent = UserConsent::new(resource.clone(), fp.clone(), SystemTime::UNIX_EPOCH);
        let decision = || PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::EvidenceFreshAndComplete],
            evidence_collected_at: SystemTime::UNIX_EPOCH,
            evaluated_at: SystemTime::UNIX_EPOCH,
            policy_version: 1,
        };

        // Matching case: original and decoded fingerprints must both
        // authorize against the same consent.
        let approval_with_original = authorize(decision(), fp.clone(), Some(&consent));
        let approval_with_decoded = authorize(decision(), decoded, Some(&consent));
        assert!(approval_with_original.is_some());
        assert!(approval_with_decoded.is_some());

        // Non-matching case: a decoded fingerprint for a *different* real
        // file must refuse authorization exactly like the original
        // `other_fp` would — proving the decoded value isn't just always
        // "close enough" to pass.
        let other_token = encode_fingerprint_token(&other_fp);
        let other_decoded = decode_fingerprint_token(&other_token).expect("token should decode");
        let approval_with_original_mismatch = authorize(decision(), other_fp, Some(&consent));
        let approval_with_decoded_mismatch = authorize(decision(), other_decoded, Some(&consent));
        assert!(approval_with_original_mismatch.is_none());
        assert!(approval_with_decoded_mismatch.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn all_none_fingerprint_round_trips_and_reports_no_token_worthy_data() {
        let fp = ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        };
        let token = encode_fingerprint_token(&fp);
        let decoded = decode_fingerprint_token(&token).expect("token should decode");
        assert_eq!(decoded, fp);
    }

    #[test]
    fn decode_rejects_malformed_tokens() {
        assert!(decode_fingerprint_token("not-a-token").is_err());
        assert!(decode_fingerprint_token("v1:zz").is_err());
        assert!(decode_fingerprint_token("v1:0").is_err());
        assert!(decode_fingerprint_token("v2:00").is_err());
        assert!(decode_fingerprint_token("v1:").is_err());
    }
}
