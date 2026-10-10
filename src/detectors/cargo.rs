//! Cargo `target/` directory detector.
//!
//! MVP scope: this detector does not discover arbitrary project roots on
//! disk (that would require a full-disk walk, which is the scanner's job,
//! not a detector's). It only checks each root in
//! [`DiscoveryContext::known_project_roots`] for a `target/` directory —
//! callers (CLI wiring, future config) are responsible for supplying that
//! list.
//!
//! ## What this detector may conclude, and what it may not (HORO-1576)
//!
//! It never looks at cargo. It looks at directories under roots it was
//! handed, which means `tool_absent` was never a conclusion available to it
//! on any input: a machine with cargo on `PATH` and 25 GB of `target/` got
//! told the tool was not installed, purely because nobody had configured a
//! project root. The three answers it can honestly give are:
//!
//! - no roots to examine → [`DetectorStatus::NotConfigured`]. Nothing was
//!   looked at, and the fix is a setting.
//! - roots examined, no `target/` under any of them → `Found(vec![])`. The
//!   search ran and came back empty, which is the same reasoning HORO-1575
//!   applied to the shared caches.
//! - a root could not be read → [`DetectorStatus::Failed`].
//!
//! `reclaimable_bytes` reuses the same [`estimate_logical_bytes`] estimate
//! as `logical_bytes` (HORO-992): a `target/` directory is fully owned,
//! regenerable build output with no partial-retention concept, so
//! `reclaimable_bytes == logical_bytes` is an honest equivalence of
//! meaning here. The estimate recurses through the full subtree, bounded
//! by a size/time budget (HORO-1016) — see [`estimate_logical_bytes`]'s
//! own doc comment for what happens if that budget is hit before the walk
//! finishes (a truthful lower bound, never a precision guarantee).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::evidence::correlate::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::evidence::{
    CargoTargetScope, Evidence, NativeCleanup, Recoverability, Regenerability, ResourceId,
    ResourceKind, ResourceLocator, TargetScope,
};

use super::{
    cache_root_status, discovery_evidence, estimate_logical_bytes, probe_mtime,
    size_estimate_budget, Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence,
    ToolEnvVar,
};

/// The program this module's `cargo metadata` resolver runs. Named here
/// rather than inline so [`super::SPAWNED_PROGRAMS`] can be built from the
/// detectors' own declarations instead of a second list that could drift
/// from them (HORO-1560).
pub(super) const CARGO_PROGRAM: &str = "cargo";

/// The program [`resolve_trusted_toolchain`] runs to pin a trusted
/// toolchain before any `cargo metadata` call against an untrusted
/// `project_root` cwd. Named here for the same reason as
/// [`CARGO_PROGRAM`] (HORO-1560).
pub(super) const RUSTUP_PROGRAM: &str = "rustup";

/// Wall-clock budget for one `cargo metadata` resolution (HORO-1824 §4.2).
/// Run under the crate's existing timeout runner, same pattern as every
/// other real-subprocess probe (`lsof`, `git`).
const CARGO_METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// Wall-clock budget for resolving the trusted toolchain pin (below). Kept
/// short and separate from [`CARGO_METADATA_TIMEOUT`] — this runs `rustup`,
/// not `cargo`, and from a neutral cwd, so it should never be slow.
const RUSTUP_TOOLCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolves and caches the toolchain Glomeris's OWN process would
/// ordinarily use, from a NEUTRAL current directory (`$HOME`) — never from
/// `project_root`.
///
/// # Why this exists (security)
///
/// `cargo` on a rustup-managed machine is rustup's proxy binary, not the
/// real `cargo` — and rustup's proxy inspects the CURRENT DIRECTORY AND
/// ITS ANCESTORS for a `rust-toolchain`/`rust-toolchain.toml` override
/// *before* invoking the real toolchain. [`run_cargo_metadata`] is run
/// with `current_dir(project_root)`, and `project_root` is a Glomeris
/// *configured* root — not necessarily something Glomeris's own user
/// trusts the CONTENTS of (it could be a cloned repo, a vendored
/// dependency checkout, anything pointed at by configuration). Without
/// this pin, a crafted `rust-toolchain.toml` sitting in or above
/// `project_root` could make a read-only evidence probe silently select —
/// and on some rustup versions/override shapes, install or otherwise
/// execute — an attacker-chosen toolchain, which is a real
/// code-execution-via-untrusted-config risk, not merely a correctness one.
///
/// `RUSTUP_TOOLCHAIN=<this value>` set on the `cargo metadata` subprocess
/// (see [`run_cargo_metadata`]) makes rustup use exactly this toolchain
/// and ignore any override file under `project_root`, while still
/// resolving `cargo metadata`'s real config-resolved answer — the whole
/// point of this probe. `RUSTUP_AUTO_INSTALL=0` (already set) is kept as
/// defense in depth, not relied on alone: it stops auto-install on
/// versions that still read the override file, but this pin stops the
/// override file from being read for its *toolchain selection* at all.
///
/// Resolved ONCE per process (via `rustup show active-toolchain`, which
/// itself is run from `$HOME` so it cannot be influenced by any
/// `project_root`'s own `rust-toolchain.toml` either) and cached — this is
/// deliberately not a per-probe subprocess. `None` means no trusted pin
/// could be established (rustup absent, timed out, or failed); every
/// caller MUST fail closed in that case rather than running `cargo
/// metadata` against an untrusted cwd with no toolchain pin at all.
///
/// `home` is injected, never read from the ambient process environment
/// here (HORO-1543: a detector that reads `$HOME` itself answers from the
/// real host machine even under a fixture [`DiscoveryContext`]) — callers
/// pass [`DiscoveryContext::home_dir`] in production and a fixture
/// directory in tests.
fn resolve_trusted_toolchain(home: &Path) -> Option<String> {
    static TOOLCHAIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TOOLCHAIN
        .get_or_init(|| {
            // This tool has to be asked: `rustup show active-toolchain`
            // is the only documented way to learn which toolchain this
            // process would ordinarily select — there is no config file
            // Glomeris can read instead, and reading `rustup`'s own
            // internal override-resolution state file directly would be
            // exactly the fragile, version-coupled reimplementation
            // ADR-0001 §10 alternative 8 rejects for Cargo's own config.
            // Run from `$HOME` (never `project_root`) specifically so
            // THIS probe cannot itself be steered by an untrusted
            // `rust-toolchain.toml`.
            let mut command = Command::new(RUSTUP_PROGRAM);
            command
                .arg("show")
                .arg("active-toolchain")
                .current_dir(home);
            let output = match run_with_timeout(command, RUSTUP_TOOLCHAIN_TIMEOUT) {
                CommandOutcome::Completed(output) if output.status.success() => output,
                _ => return None,
            };
            let stdout = String::from_utf8(output.stdout).ok()?;
            // Real format observed: "stable-aarch64-apple-darwin (default)\n"
            // — the toolchain name is the first whitespace-delimited token.
            let name = stdout.split_whitespace().next()?;
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .clone()
}

/// Raw fields read out of `cargo metadata --format-version 1 --no-deps
/// --offline`'s JSON. Intentionally narrow — this never attempts to
/// deserialize the (enormous, version-sensitive) full metadata shape,
/// only the three fields ADR-0001 §4.2 names.
struct CargoMetadataFields {
    target_directory: PathBuf,
    /// `None` when the field is absent (older cargo) OR equal to
    /// `target_directory` (the common case) — report-only either way,
    /// per the ADR.
    build_directory: Option<PathBuf>,
    workspace_root: PathBuf,
}

/// Runs `cargo metadata --format-version 1 --no-deps --offline` with
/// `current_dir = project_root`, per ADR-0001 §4.2.
///
/// Environment: `RUSTUP_AUTO_INSTALL=0`, and `CARGO_TARGET_DIR`/
/// `CARGO_BUILD_TARGET_DIR` stripped so this reports Cargo's
/// CONFIG-resolved answer, never Glomeris's own (possibly different)
/// process environment. `cargo_home_override` — when `Some`, from
/// [`DiscoveryContext::tool_env`]`(ToolEnvVar::CargoHome)` — lets a
/// hermetic test or an explicit `CARGO_HOME` override reach the
/// subprocess the same way [`super::cargo_registry_dir`] already reaches
/// `CargoRegistryCacheDetector`; `None` means inherit whatever
/// `CARGO_HOME` Glomeris's own process already has (production default).
///
/// Returns `None` on ANY failure — absent binary, timeout, non-zero
/// exit, unparseable JSON, or a required field missing from the JSON.
/// Never guesses a default: an empty/synthetic answer here would let a
/// probe failure silently resolve to `Local`, exactly the fail-open
/// pattern ADR-0001's non-negotiable section forbids.
fn run_cargo_metadata(
    project_root: &Path,
    cargo_home_override: Option<&str>,
    home: &Path,
) -> Option<CargoMetadataFields> {
    // Security (flagged by independent review): `cargo` here is rustup's
    // proxy, and rustup resolves a `rust-toolchain`/`rust-toolchain.toml`
    // override from `current_dir` and its ANCESTORS before invoking the
    // real toolchain. `project_root` is a Glomeris-configured root, not
    // necessarily a root whose CONTENTS are trusted — a crafted override
    // file there could otherwise steer this probe into selecting (and on
    // some rustup versions/override shapes, installing or executing) an
    // attacker-chosen toolchain. `RUSTUP_TOOLCHAIN` pinned to a toolchain
    // resolved from a NEUTRAL cwd makes rustup ignore any override under
    // `project_root` entirely. A pin that cannot be established is not a
    // reason to proceed without one.
    let trusted_toolchain = resolve_trusted_toolchain(home)?;

    // This tool has to be asked: no documented config file or env var
    // exposes Cargo's fully-merged, precedence-resolved target directory
    // (global config, nested config, `--config`, `CARGO_TARGET_DIR`, a
    // workspace override) — only `cargo metadata` itself applies Cargo's
    // own merge rules, which is exactly ADR-0001 §10 alternative 8's
    // rationale for not hand-parsing `.cargo/config.toml`. When asked, it
    // only reads config/manifest files and prints JSON describing them —
    // `--offline`/`--no-deps` keep it from touching the network or
    // resolving the dependency graph, and it writes no file (verified
    // empirically against a real fixture; see the PR body).
    let mut command = Command::new(CARGO_PROGRAM);
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--offline")
        .current_dir(project_root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("RUSTUP_TOOLCHAIN", &trusted_toolchain);
    if let Some(home) = cargo_home_override {
        command.env("CARGO_HOME", home);
    }

    let output = match run_with_timeout(command, CARGO_METADATA_TIMEOUT) {
        CommandOutcome::Completed(output) if output.status.success() => output,
        // NotFound (cargo absent), TimedOut, SpawnFailed, or a non-zero
        // exit (e.g. no Cargo.toml at `project_root`, or a cargo safety
        // refusal) all fail closed here — none of them may be read as
        // "no target directory", only as "unknown".
        _ => return None,
    };

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let target_directory = json.get("target_directory")?.as_str()?;
    let workspace_root = json.get("workspace_root")?.as_str()?;
    let build_directory = json
        .get("build_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    Some(CargoMetadataFields {
        target_directory: PathBuf::from(target_directory),
        build_directory,
        workspace_root: PathBuf::from(workspace_root),
    })
}

/// Resolves [`CargoTargetScope`] for one discovered Cargo target
/// directory (HORO-1824 §4.2).
///
/// `locator` is the resource's own (already-canonicalized) path — the
/// `<root>/target` the detector found, or the resolved target a
/// discovery-widening pass is attaching scope to. Comparing it against
/// cargo's own `target_directory` is what distinguishes `Local` from
/// `Orphan`: both have `resolved_target` under `workspace_root`, but
/// `Orphan` means the root *searched* (`<root>/target`) is not the
/// directory cargo would actually use.
///
/// Every path comparison here is canonicalized on both sides before
/// comparing — never raw string equality, which on macOS would read
/// `/var/...` and `/private/var/...` as different directories when they
/// are the same one.
///
/// `home` is passed straight through to [`resolve_trusted_toolchain`] —
/// see its doc comment for why it must be injected rather than read from
/// the ambient environment here.
pub(crate) fn resolve_cargo_target_scope(
    project_root: &Path,
    locator: &Path,
    cargo_home_override: Option<&str>,
    home: &Path,
) -> ProbeOutcome<CargoTargetScope> {
    let Some(fields) = run_cargo_metadata(project_root, cargo_home_override, home) else {
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    };

    let Ok(resolved_target) = fields.target_directory.canonicalize() else {
        // Cargo named a target_directory that cannot itself be
        // canonicalized (e.g. a dangling component) — not observable,
        // fail closed rather than compare against a raw, unverified path.
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    };
    let Ok(workspace_root) = fields.workspace_root.canonicalize() else {
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    };
    let resolved_build_dir = fields
        .build_directory
        .and_then(|b| b.canonicalize().ok())
        .filter(|b| *b != resolved_target);
    let locator_canonical = locator
        .canonicalize()
        .unwrap_or_else(|_| locator.to_path_buf());

    // Security (flagged by independent review): `target_directory` comes
    // from `cargo metadata`, which applies WHATEVER `.cargo/config.toml`
    // the configured root (or a config above it) carries — a root
    // Glomeris was told to look at, not necessarily one whose config
    // Glomeris's user authored or reviewed. A misconfigured or crafted
    // `build.target-dir` (`".."`, `"."`, an absolute path at the
    // workspace root or an ancestor of it) can make `resolved_target`
    // equal or contain the workspace root — or any other directory that
    // demonstrably holds real source. The only Cargo mutation this
    // crate ever runs is `cargo clean --target-dir <resolved_target>`,
    // which treats that whole directory as disposable build output; a
    // misresolved answer here must never reach a scope that could become
    // AUTO_SAFE or be pre-authorized; it must be reported as UNKNOWN
    // (fail-closed `TargetOwnershipUnknown`, not any flavor of "safe"),
    // never silently narrowed by picking the "least bad" scope value.
    if resolved_target == workspace_root
        || workspace_root.starts_with(&resolved_target)
        || resolved_target.join("Cargo.toml").is_file()
        || has_rust_source(&resolved_target.join("src"))
    {
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    }

    let scope = if resolved_target.starts_with(&workspace_root) {
        if locator_canonical == resolved_target {
            TargetScope::Local
        } else {
            // Under the workspace root, but not the directory actually
            // searched — a pre-shared-config leftover.
            TargetScope::Orphan
        }
    } else {
        TargetScope::Redirected
    };

    // Security (flagged by independent review): computed fresh from
    // `locator`/`project_root` on EVERY call — discovery AND revalidation
    // alike — rather than carried forward as remembered state. A resource
    // surfaced by discovery widening always has `locator == resolved_target`
    // by construction (that is what widening means), which makes `scope`
    // alone an unsafe signal for it: re-resolving from the SAME
    // `project_root` at execution time would trivially recompute `Local`
    // whenever `resolved_target` happens to sit under that root's
    // workspace, silently erasing the `Orphan`/`Redirected` judgment
    // discovery made. `is_direct_locator` cannot erode the same way: a
    // widened resource's `locator` is NEVER `<project_root>/target`
    // (widening only fires when it differs — see `CargoDetector::discover`),
    // so this is `false` for it on every call, forever, independent of
    // `scope`'s own value at that moment. See `classify()`'s own use of
    // this field for the resulting policy rule.
    // Canonicalized on both sides, same reasoning as every other
    // comparison in this function: `project_root` itself may be a
    // non-canonical path (e.g. `/tmp/...` where the real filesystem
    // entry is `/private/tmp/...` on macOS), and comparing a canonical
    // `locator_canonical` against a non-canonical `project_root.join(...)`
    // would read the same directory as two different ones.
    let project_root_canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let is_direct_locator = locator_canonical == project_root_canonical.join("target");

    ProbeOutcome::Observed(CargoTargetScope {
        resolved_target,
        resolved_build_dir,
        workspace_root,
        scope,
        is_direct_locator,
    })
}

/// Whether `src_dir` contains at least one `*.rs` file directly inside it
/// — the cheap, bounded signal that a candidate "target directory" is
/// actually a source tree (part of the implausible-target guard above).
/// Deliberately shallow (one `read_dir`, no recursion): this exists only
/// to catch the gross misconfiguration case, not to re-implement a
/// project-root detector.
fn has_rust_source(src_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(src_dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
    })
}

/// Protectiveness ranking for [`TargetScope`] — higher is more protective.
/// Used ONLY to pick which scope wins when discovery widening deduplicates
/// multiple configured roots onto the same physical (dev, ino) (ADR-0001
/// §4.2: "dedup takes the most-protective scope, not the first-discovered
/// one"). Never a policy input on its own — `classify` reads
/// `TargetScope` directly, not this ranking.
fn scope_rank(scope: TargetScope) -> u8 {
    match scope {
        TargetScope::Local => 0,
        TargetScope::Orphan => 1,
        TargetScope::Redirected => 2,
    }
}

/// Inserts or merges one discovery-widening candidate for the resolved
/// target identified by `key` (its (dev, ino)).
///
/// First root to reach a given `key` builds the widened [`Evidence`] (its
/// own estimate, its own `source_project_root` — "used only for
/// `--manifest-path`", per the ADR, so the FIRST root's manifest lookup is
/// the one that is actually used later). Every subsequent root that
/// resolves to the same `key` only ever contributes its own scope
/// comparison: if its OWN independent resolution outranks the
/// already-merged scope, the merged entry's scope is raised. A root whose
/// own resolution happens to be `Local` must never mask a second root
/// that independently redirects into the same physical directory — this
/// is the exact bug HORO-1822's own review round found and the ADR
/// documents.
fn merge_widened_scope(
    widened: &mut HashMap<(u64, u64), Evidence>,
    key: (u64, u64),
    root: PathBuf,
    resolved_target: PathBuf,
    scope_outcome: ProbeOutcome<CargoTargetScope>,
    detector_id: DetectorId,
) {
    let Some(incoming_scope) = scope_outcome.observed().map(|s| s.scope) else {
        return;
    };

    if let Some(existing) = widened.get_mut(&key) {
        let existing_scope = match &existing.cargo_target_scope {
            Some(ProbeOutcome::Observed(s)) => s.scope,
            _ => return,
        };
        if scope_rank(incoming_scope) > scope_rank(existing_scope) {
            if let Some(ProbeOutcome::Observed(s)) = &mut existing.cargo_target_scope {
                s.scope = incoming_scope;
            }
        }
        return;
    }

    let estimate = estimate_logical_bytes(&resolved_target, size_estimate_budget());
    let logical_bytes = estimate.bytes.clone();
    let resource = ResourceId::new(
        ResourceKind::CargoTargetDir,
        ResourceLocator::Path(resolved_target.clone()),
    )
    .with_source_project_root(root);
    let mut ev = discovery_evidence(
        resource,
        detector_id,
        &resolved_target,
        logical_bytes.clone(),
        logical_bytes,
        estimate.is_lower_bound(),
        probe_mtime(&resolved_target),
        Regenerability::RegenerableByRebuild,
        Recoverability::RegenerableByRebuild,
        NativeCleanup::Unsupported,
    );
    if let Some(note) = estimate.lower_bound_note() {
        ev.push_source(note);
    }
    ev.cargo_target_scope = Some(scope_outcome);
    widened.insert(key, ev);
}

pub struct CargoDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::CargoTargetDir];

impl Detector for CargoDetector {
    fn id(&self) -> DetectorId {
        DetectorId("cargo_target_dir")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        // Before anything is read: with no roots, the loop below would run
        // zero times and every answer after it would be about a search that
        // never happened (HORO-1576).
        if ctx.known_project_roots.is_empty() {
            return DetectorStatus::NotConfigured;
        }

        // Same hermeticity rule `cargo_registry_dir` already applies: fall
        // back to `<home_dir>/.cargo`, never silently inherit whatever
        // CARGO_HOME this Glomeris process happens to have ambiently. A
        // hermetic `DiscoveryContext::new(fixture_home)` test must not
        // reach the developer's real `~/.cargo/config.toml` — a
        // nonexistent `<fixture_home>/.cargo` is a normal, safe override
        // (verified: cargo tolerates a CARGO_HOME that doesn't exist and
        // falls back to its own built-in defaults, creating nothing).
        let cargo_home_override_owned = ctx
            .tool_env(ToolEnvVar::CargoHome)
            .map(str::to_string)
            .unwrap_or_else(|| ctx.home_dir.join(".cargo").to_string_lossy().into_owned());
        let cargo_home_override = Some(cargo_home_override_owned.as_str());

        let mut evidence = Vec::new();
        let mut saw_permission_error = false;
        // HORO-1824 §4.2 discovery widening: keyed by the resolved target's
        // (dev, ino), so a second configured root resolving to the SAME
        // physical directory (via a shared/global config) merges rather
        // than double-counting. "Dedup takes the most-protective scope,
        // not the first-discovered one" — see `merge_widened_scope` below.
        let mut widened: HashMap<(u64, u64), Evidence> = HashMap::new();

        for root in &ctx.known_project_roots {
            let target_dir: PathBuf = root.join("target");
            match target_dir.canonicalize() {
                Ok(canonical) => {
                    let resource = ResourceId::new(
                        ResourceKind::CargoTargetDir,
                        ResourceLocator::Path(canonical.clone()),
                    )
                    .with_source_project_root(root.clone());
                    let estimate = estimate_logical_bytes(&canonical, size_estimate_budget());
                    let logical_bytes = estimate.bytes.clone();
                    let mut ev = discovery_evidence(
                        resource,
                        self.id(),
                        &canonical,
                        logical_bytes.clone(),
                        logical_bytes,
                        estimate.is_lower_bound(),
                        probe_mtime(&canonical),
                        Regenerability::RegenerableByRebuild,
                        Recoverability::RegenerableByRebuild,
                        NativeCleanup::Unsupported,
                    );
                    if let Some(note) = estimate.lower_bound_note() {
                        ev.push_source(note);
                    }

                    let scope_outcome = resolve_cargo_target_scope(
                        root,
                        &canonical,
                        cargo_home_override,
                        &ctx.home_dir,
                    );
                    ev.cargo_target_scope = Some(scope_outcome.clone());

                    // Discovery widening: when cargo's own resolution lands
                    // somewhere other than `<root>/target`, that somewhere
                    // is a resource this detector would otherwise never
                    // surface — most consequentially a shared/redirected
                    // target holding a live installed executable
                    // dependency (HORO-1825's guard only runs on resources
                    // that are actually discovered). Skipped entirely when
                    // the resolver itself failed: widening onto an
                    // unresolved path would be a guess, not a discovery.
                    if let ProbeOutcome::Observed(scope) = &scope_outcome {
                        if scope.resolved_target != canonical {
                            if let Ok(resolved_meta) = std::fs::metadata(&scope.resolved_target) {
                                use std::os::unix::fs::MetadataExt;
                                let key = (resolved_meta.dev(), resolved_meta.ino());
                                merge_widened_scope(
                                    &mut widened,
                                    key,
                                    root.clone(),
                                    scope.resolved_target.clone(),
                                    scope_outcome.clone(),
                                    self.id(),
                                );
                            }
                        }
                    }

                    evidence.push(ev);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    saw_permission_error = true;
                }
                Err(_) => continue,
            }
        }

        evidence.extend(widened.into_values());

        if evidence.is_empty() && saw_permission_error {
            return DetectorStatus::Failed(
                "permission denied probing one or more known project roots".to_string(),
            );
        }

        // Empty is `Found(vec![])`, never `ToolAbsent`: roots were examined and
        // none held a `target/`. This detector has made no observation about
        // cargo and is not entitled to a claim about it (HORO-1575/1576).
        DetectorStatus::Found(evidence)
    }
}

/// Detector for the shared Cargo registry cache (HORO-1543).
///
/// The resource is `<cargo home>/registry`, and deliberately never the Cargo
/// home itself. `$CARGO_HOME` also holds `credentials.toml` — registry
/// publish tokens — and `bin/`, the binaries `cargo install` put on the
/// user's `PATH`. Naming the parent would put both inside a reclaimable
/// resource, and no later policy class could make that safe again.
///
/// `<cargo home>/git` is also deliberately excluded. It holds checkouts of
/// git dependencies, which can pin a commit that has since been force-pushed
/// away — the same unprovable-reproducibility problem as Maven's local
/// repository, and not something this ticket's evidence can settle. It is
/// simply not claimed rather than claimed with a guess.
pub struct CargoRegistryCacheDetector;

const REGISTRY_KINDS: &[ResourceKind] = &[ResourceKind::CargoRegistryCache];

/// The one subdirectory of the Cargo home this detector will ever name. See
/// [`CargoRegistryCacheDetector`] for the two it must not.
const REGISTRY_SUBDIR: &str = "registry";

/// Resolves the registry cache root from `CARGO_HOME` and `$HOME`, applying
/// Cargo's own two rules.
///
/// Pure so it is testable: the `CARGO_HOME` override arrives through
/// [`DiscoveryContext::tool_env`] rather than from the process environment,
/// so a fixture context can exercise both rules. Reading it here directly
/// would make this detector answer from the developer's real registry even
/// under a fixture `home_dir` — see [`super::ToolEnvVar`], which exists
/// because that is exactly what happened.
///
/// An empty or whitespace-only `CARGO_HOME` falls back to `~/.cargo` rather
/// than resolving to a relative `registry` or to `/registry` — an exported
/// but unset variable is a common shell accident, and treating it as an
/// answer would name a directory belonging to something else.
fn cargo_registry_dir(cargo_home: Option<&str>, home: &Path) -> PathBuf {
    let cargo_home = match cargo_home.map(str::trim) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".cargo"),
    };
    cargo_home.join(REGISTRY_SUBDIR)
}

impl Detector for CargoRegistryCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("cargo_registry_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        REGISTRY_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let registry = cargo_registry_dir(ctx.tool_env(ToolEnvVar::CargoHome), &ctx.home_dir);
        // `registry` is always `<cargo home>/registry`, so the parent is the
        // Cargo home — a directory only Cargo (or rustup, installing it)
        // creates.
        let cargo_home = registry
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| registry.clone());

        cache_root_status(
            self.id(),
            ResourceKind::CargoRegistryCache,
            &registry,
            // Downloaded `.crate` files and their extracted sources: cargo
            // refetches these on the next build, given network access and,
            // for a private registry, credentials — the same caveat the Go
            // module cache carries.
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "Cargo registry cache under the Cargo home \
             (CARGO_HOME if set, otherwise ~/.cargo)",
            // A present Cargo home with no `registry` means cargo is
            // installed and has not fetched a dependency yet — not that cargo
            // is missing (HORO-1575).
            RootAbsence::InferredUnderToolOwnedParent(cargo_home),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Minimal real Cargo project at `root`, so `cargo metadata` has a
    /// manifest to answer about.
    fn write_manifest(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"glomeris-cargo-scope-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
    }

    /// Pins `cargo metadata`'s target-dir resolution to `root`'s own
    /// `target/`, overriding any machine-wide `~/.cargo/config.toml`.
    /// Project-local config always wins over the user-level one.
    fn pin_locally(root: &Path) {
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"target\"\n",
        )
        .unwrap();
    }

    // ---- HORO-1824 §4.2 resolver tests ----

    /// The ordinary case: a real Cargo project with no override anywhere
    /// resolves `Local`.
    #[test]
    fn resolve_cargo_target_scope_is_local_for_an_ordinary_project() {
        let root = make_temp_dir("scope-local");
        pin_locally(&root);
        write_manifest(&root);
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();

        let outcome = resolve_cargo_target_scope(&root, &target_dir, None, Path::new("/tmp"));
        match outcome {
            ProbeOutcome::Observed(scope) => {
                assert_eq!(scope.scope, TargetScope::Local);
                assert_eq!(scope.resolved_target, target_dir.canonicalize().unwrap());
                assert_eq!(scope.workspace_root, root.canonicalize().unwrap());
            }
            other => panic!("expected Observed(Local), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// No manifest at all: `cargo metadata` fails (non-zero exit, no
    /// Cargo.toml) and the resolver must fail closed to `Unavailable`,
    /// never guess `Local`.
    #[test]
    fn resolve_cargo_target_scope_is_unavailable_with_no_manifest() {
        let root = make_temp_dir("scope-no-manifest");
        pin_locally(&root);
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();

        let outcome = resolve_cargo_target_scope(&root, &target_dir, None, Path::new("/tmp"));
        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::Failed));

        fs::remove_dir_all(&root).ok();
    }

    /// `target_override` fixture (AC list): a project-local
    /// `.cargo/config.toml` redirecting `target-dir` OUTSIDE the
    /// workspace root resolves `Redirected` — the shared/global-config
    /// shape this ticket exists to catch, reproduced locally so the test
    /// never depends on the developer's real `~/.cargo/config.toml`.
    #[test]
    fn resolve_cargo_target_scope_is_redirected_when_config_points_outside_workspace() {
        let root = make_temp_dir("scope-redirected-root");
        write_manifest(&root);
        let shared = make_temp_dir("scope-redirected-shared");
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            format!(
                "[build]\ntarget-dir = \"{}\"\n",
                shared.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        let locator = root.join("target"); // never created — Redirected means Cargo never put anything here
        let outcome = resolve_cargo_target_scope(&root, &locator, None, Path::new("/tmp"));
        match outcome {
            ProbeOutcome::Observed(scope) => {
                assert_eq!(scope.scope, TargetScope::Redirected);
                assert_eq!(scope.resolved_target, shared.canonicalize().unwrap());
            }
            other => panic!("expected Observed(Redirected), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&shared).ok();
    }

    /// `parent_repo` / stale-leftover fixture: the locator (`<root>/target`)
    /// is a real, pre-existing directory, but the root's OWN config
    /// redirects elsewhere — the configured root no longer shares the
    /// directory sitting at the locator. `Orphan`, not `Local`.
    #[test]
    fn resolve_cargo_target_scope_is_orphan_when_root_no_longer_shares_the_locator() {
        let root = make_temp_dir("scope-orphan-root");
        write_manifest(&root);
        // A NESTED config redirects to a different directory NAME, but
        // one that is still under the SAME workspace root — e.g. a
        // project that switched its local target-dir from `target` to
        // `build`. `resolved_target` (`<root>/build`) stays under
        // `workspace_root` (`<root>`), which is what distinguishes this
        // from `Redirected` (resolved_target OUTSIDE workspace_root).
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"build\"\n",
        )
        .unwrap();
        // A leftover `target/` directory still sits under `root` even
        // though cargo itself now resolves to `<root>/build`.
        let stale_locator = root.join("target");
        fs::create_dir_all(&stale_locator).unwrap();
        fs::create_dir_all(root.join("build")).unwrap();

        let outcome = resolve_cargo_target_scope(&root, &stale_locator, None, Path::new("/tmp"));
        match outcome {
            ProbeOutcome::Observed(scope) => {
                assert_eq!(scope.scope, TargetScope::Orphan);
                // Even though `scope` is the "safe" Orphan value, this
                // locator is NOT the plain `<root>/target` cargo would
                // actually use — wait, it literally IS `<root>/target`
                // here, so it IS the direct locator; the point of this
                // fixture is Orphan, not widening. `is_direct_locator`
                // is about whether the EVIDENCE's own locator equals
                // `<project_root>/target`, which is true here regardless
                // of where cargo itself resolved to.
                assert!(scope.is_direct_locator);
            }
            other => panic!("expected Observed(Orphan), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// Security regression (independent review finding): a `project_root`
    /// whose contents are not trusted could carry a crafted
    /// `rust-toolchain.toml` naming a toolchain that is not installed.
    /// `current_dir(project_root)` alone would let rustup's `cargo` proxy
    /// read that override; the resolver must pin `RUSTUP_TOOLCHAIN` from
    /// a neutral cwd so the bogus override is never consulted, and the
    /// probe must still resolve correctly rather than hang, error, or
    /// attempt to install the bogus toolchain.
    #[test]
    fn resolve_cargo_target_scope_ignores_a_bogus_rust_toolchain_override() {
        let root = make_temp_dir("scope-bogus-toolchain");
        pin_locally(&root);
        write_manifest(&root);
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"nightly-1970-01-01-bogus-nonexistent\"\n",
        )
        .unwrap();
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();

        let outcome = resolve_cargo_target_scope(&root, &target_dir, None, Path::new("/tmp"));

        match outcome {
            ProbeOutcome::Observed(scope) => assert_eq!(scope.scope, TargetScope::Local),
            other => panic!(
                "expected Observed(Local) despite the bogus toolchain override, got {other:?} \
                 — the RUSTUP_TOOLCHAIN pin did not take effect"
            ),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// Security regression (independent review finding #2): a misconfigured
    /// or crafted `target-dir` pointing at the workspace root ITSELF must
    /// never resolve to any scope at all — `Local` would make it
    /// `AUTO_SAFE`-eligible, and the only mutation this crate ever runs
    /// against a Cargo target dir (`cargo clean --target-dir <resolved>`)
    /// would then be pointed at a directory holding real, unpushed source.
    #[test]
    fn resolve_cargo_target_scope_rejects_target_dir_equal_to_workspace_root() {
        let root = make_temp_dir("scope-implausible-root-itself");
        write_manifest(&root);
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \".\"\n",
        )
        .unwrap();

        let outcome = resolve_cargo_target_scope(&root, &root, None, Path::new("/tmp"));
        assert_eq!(
            outcome,
            ProbeOutcome::Unavailable(ProbeReason::Failed),
            "target-dir resolving to the workspace root itself must be rejected, never scoped"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// Security regression (independent review finding #2), second shape: a
    /// `target-dir` resolving to a directory that itself holds a nested
    /// `Cargo.toml` and real `.rs` source — implausible as build output,
    /// must also be rejected outright rather than scoped `Local`/`Orphan`.
    #[test]
    fn resolve_cargo_target_scope_rejects_target_dir_containing_real_source() {
        let root = make_temp_dir("scope-implausible-source-root");
        write_manifest(&root);
        let decoy_source = make_temp_dir("scope-implausible-source-decoy");
        write_manifest(&decoy_source);
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            format!(
                "[build]\ntarget-dir = \"{}\"\n",
                decoy_source.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        let locator = root.join("target");
        let outcome = resolve_cargo_target_scope(&root, &locator, None, Path::new("/tmp"));
        assert_eq!(
            outcome,
            ProbeOutcome::Unavailable(ProbeReason::Failed),
            "a resolved target directory holding a nested Cargo.toml/src/*.rs must be rejected"
        );

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&decoy_source).ok();
    }

    /// End-to-end proof that the rejection above actually reaches a
    /// non-preauthorizable, non-AUTO_SAFE classification — not merely a
    /// scope-field assertion. `Unavailable` makes `TargetOwnership`
    /// missing, which `classify()` maps to `Ask{TargetOwnershipUnknown}`
    /// (never the generic `EvidenceIncomplete`, see `classify`'s own
    /// comment), and `TargetOwnershipUnknown` can never be
    /// pre-authorized.
    #[test]
    fn an_implausible_target_dir_never_reaches_auto_safe_or_preauthorization() {
        use crate::evidence::{Completeness, EvidenceField};
        use crate::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};

        let root = make_temp_dir("scope-implausible-end-to-end");
        write_manifest(&root);
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \".\"\n",
        )
        .unwrap();

        let outcome = resolve_cargo_target_scope(&root, &root, None, Path::new("/tmp"));
        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::Failed));

        // Build otherwise-complete evidence (every other required field
        // Observed) so the ONLY missing field is `TargetOwnership` —
        // isolating exactly the behavior this regression is about.
        use crate::evidence::{
            Evidence, NativeCleanup, Recoverability, ResourceFingerprint, ResourceId,
            ResourceLocator,
        };
        let now = SystemTime::now();
        let ev = Evidence {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(root.clone()),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: super::DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(0),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(0),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(now),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::CargoTargetDir.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Observed(
                crate::evidence::ExecutableDependencyReport::empty(),
            ),
            cargo_target_scope: Some(outcome),
            collected_at: now,
            sources: Vec::new(),
        };
        assert_eq!(
            ev.completeness(),
            Completeness::Partial {
                missing: vec![EvidenceField::TargetOwnership]
            }
        );

        let decision = classify(&ev, &PolicyConfig::default(), ev.collected_at);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::TargetOwnershipUnknown]);
        assert!(
            !crate::autopilot::envelope::is_preauthorizable(ReasonCode::TargetOwnershipUnknown),
            "TargetOwnershipUnknown must never be pre-authorizable"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// With nothing configured, nothing was examined — and that is a
    /// different answer from either "cargo is missing" or "nothing is there"
    /// (HORO-1576).
    ///
    /// Mutation control: restoring `ToolAbsent` here makes this fail, and it
    /// is the assertion that was wrong before this ticket. A machine with
    /// cargo installed and gigabytes of build output got `tool_absent` from
    /// this exact input.
    #[test]
    fn no_known_project_roots_examined_nothing() {
        let ctx = DiscoveryContext::new("/tmp");
        assert_eq!(CargoDetector.discover(&ctx), DetectorStatus::NotConfigured);
    }

    /// The discriminating pair to the test above, differing in exactly one
    /// thing — whether a root was configured. A root that holds no `target/`
    /// is a completed search, so it reports an empty result rather than
    /// "nothing was examined", and never a claim about cargo.
    #[test]
    fn a_configured_root_without_a_target_dir_searched_and_found_nothing() {
        let root = make_temp_dir("cargo-no-target");
        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);

        let status = CargoDetector.discover(&ctx);
        fs::remove_dir_all(&root).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "a missing target/ must not become a zero-byte resource"
            ),
            DetectorStatus::NotConfigured => {
                panic!("a root WAS configured and examined, so this search did happen")
            }
            other => panic!("expected Found(empty), got {other:?}"),
        }
    }

    #[test]
    fn project_root_with_target_dir_yields_evidence() {
        let root = make_temp_dir("cargo-with-target");
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/build_output.bin"), vec![0u8; 4096]).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = CargoDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].resource.kind, ResourceKind::CargoTargetDir);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1017: the discovered `Evidence`'s `resource.source_project_root`
    /// must carry the exact `known_project_roots` entry the target dir was
    /// found under, so a later action can bind manifest lookup to the
    /// real discovered root instead of re-deriving it from the
    /// (possibly symlink-resolved) canonicalized target path.
    #[test]
    fn evidence_resource_carries_the_discovered_project_root() {
        let root = make_temp_dir("cargo-project-root-carried");
        fs::create_dir_all(root.join("target")).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = CargoDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].resource.source_project_root.as_deref(),
                    Some(root.as_path())
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn registry_detector_id_and_kinds_are_stable() {
        assert_eq!(
            CargoRegistryCacheDetector.id(),
            DetectorId("cargo_registry_cache")
        );
        assert_eq!(
            CargoRegistryCacheDetector.resource_kinds(),
            &[ResourceKind::CargoRegistryCache]
        );
    }

    #[test]
    fn cargo_home_wins_over_the_default() {
        assert_eq!(
            cargo_registry_dir(Some("/opt/cargo"), Path::new("/Users/dev")),
            PathBuf::from("/opt/cargo/registry")
        );
        assert_eq!(
            cargo_registry_dir(None, Path::new("/Users/dev")),
            PathBuf::from("/Users/dev/.cargo/registry")
        );
    }

    /// An exported-but-empty `CARGO_HOME` must not become an answer: `""`
    /// would otherwise resolve to `registry` (relative) or `/registry`.
    #[test]
    fn an_empty_cargo_home_falls_back_rather_than_naming_a_stray_path() {
        for value in ["", "   ", "\t"] {
            assert_eq!(
                cargo_registry_dir(Some(value), Path::new("/Users/dev")),
                PathBuf::from("/Users/dev/.cargo/registry"),
                "CARGO_HOME={value:?} should have fallen back"
            );
        }
    }

    /// The credential/binary-safety invariant, asserted rather than
    /// commented: the resolved root is never the Cargo home, under either
    /// rule.
    #[test]
    fn the_cargo_home_itself_is_never_the_resource() {
        let from_env = cargo_registry_dir(Some("/opt/cargo"), Path::new("/Users/dev"));
        assert_ne!(from_env, PathBuf::from("/opt/cargo"));
        assert!(from_env.ends_with(REGISTRY_SUBDIR));

        let from_home = cargo_registry_dir(None, Path::new("/Users/dev"));
        assert_ne!(from_home, PathBuf::from("/Users/dev/.cargo"));
        assert!(from_home.ends_with(REGISTRY_SUBDIR));
    }

    /// Positive control for the same invariant end to end: a Cargo home
    /// holding `credentials.toml`, an installed binary, a git-dependency
    /// checkout and `registry/` yields evidence measuring only `registry/`.
    /// If the detector ever named the parent, the observed byte count would
    /// include the publish token and this assertion would fail.
    #[test]
    fn credentials_binaries_and_git_checkouts_are_not_part_of_the_resource() {
        let home = make_temp_dir("cargo-home-fixture");
        let cargo_home = home.join(".cargo");
        fs::create_dir_all(cargo_home.join("registry/cache/index.crates.io-1234")).unwrap();
        fs::create_dir_all(cargo_home.join("bin")).unwrap();
        fs::create_dir_all(cargo_home.join("git/db/some-dep-5678")).unwrap();
        fs::write(cargo_home.join("credentials.toml"), vec![b'x'; 200]).unwrap();
        fs::write(cargo_home.join("bin/some-tool"), vec![0u8; 100_000]).unwrap();
        fs::write(
            cargo_home.join("git/db/some-dep-5678/packed-refs"),
            vec![0u8; 700],
        )
        .unwrap();
        fs::write(
            cargo_home.join("registry/cache/index.crates.io-1234/serde-1.0.0.crate"),
            vec![0u8; 8_192],
        )
        .unwrap();

        // The real `discover`, through a hermetic `DiscoveryContext`: with no
        // `CargoHome` tool home set, the detector resolves from `home_dir`
        // and cannot see this machine's own registry (HORO-1543).
        match CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home)) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(8_192),
                    "only the registry subtree may be measured"
                );
                match &ev.resource.locator {
                    ResourceLocator::Path(named) => {
                        assert!(named.ends_with(REGISTRY_SUBDIR));
                        assert!(!named.ends_with(".cargo"));
                    }
                    other => panic!("expected a path locator, got {other:?}"),
                }
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&home).ok();
    }

    /// No Cargo home at all reports nothing, never a zero-byte resource.
    #[test]
    fn no_cargo_home_is_tool_absent_not_zero_bytes() {
        let home = make_temp_dir("cargo-home-empty");
        assert_eq!(
            CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home)),
            DetectorStatus::ToolAbsent
        );
        fs::remove_dir_all(&home).ok();
    }

    /// The discriminating pair to the test above: same missing `registry`,
    /// but `~/.cargo` is there. Cargo is installed, so `tool_absent` would be
    /// false (HORO-1575). The fixtures differ in exactly one thing — whether
    /// `.cargo` exists.
    #[test]
    fn a_cargo_home_with_no_registry_yet_is_not_a_missing_cargo() {
        let home = make_temp_dir("cargo-home-no-registry");
        fs::create_dir_all(home.join(".cargo")).unwrap();
        // `bin/` is what rustup writes when it installs the toolchain, so a
        // Cargo home in this shape is the normal pre-first-build state.
        fs::create_dir_all(home.join(".cargo/bin")).unwrap();

        let status = CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home));
        fs::remove_dir_all(&home).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "an unfetched registry must not become a zero-byte resource"
            ),
            DetectorStatus::ToolAbsent => {
                panic!("`~/.cargo` exists, so cargo is installed; tool_absent is false")
            }
            other => panic!("expected Found(empty), got {other:?}"),
        }
    }

    /// The `CargoHome` override has to reach `discover`, not merely
    /// `cargo_registry_dir`: the pure test above would still pass if
    /// `discover` ignored `ctx.tool_env` and always resolved from
    /// `home_dir`. So the fixture registry lives somewhere `home_dir` cannot
    /// reach, and `home_dir` points at a decoy that holds a *different* number
    /// of bytes — a detector reading the wrong one reports 1_024 and fails.
    #[test]
    fn the_cargo_home_override_reaches_the_detector() {
        let relocated = make_temp_dir("cargo-relocated");
        fs::create_dir_all(relocated.join("registry/cache")).unwrap();
        fs::write(
            relocated.join("registry/cache/serde.crate"),
            vec![0u8; 2_048],
        )
        .unwrap();

        let decoy_home = make_temp_dir("cargo-decoy-home");
        fs::create_dir_all(decoy_home.join(".cargo/registry")).unwrap();
        fs::write(
            decoy_home.join(".cargo/registry/decoy.crate"),
            vec![0u8; 1_024],
        )
        .unwrap();

        let ctx = DiscoveryContext::new(&decoy_home)
            .with_tool_env(ToolEnvVar::CargoHome, relocated.to_str().unwrap());

        match CargoRegistryCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(2_048),
                    "the relocated registry is the one CARGO_HOME names; \
                     1024 would mean the override never reached `discover`"
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&relocated).ok();
        fs::remove_dir_all(&decoy_home).ok();
    }
}
