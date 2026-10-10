//! HORO-1825 §4.3: read-only, bounded correlation of recognized Claude
//! Code / Codex / LaunchAgent configs (plus running processes) against one
//! resource, to answer "is anything configured to exec a file inside
//! this disposable cache on its next invocation, or running from it right
//! now".
//!
//! # Read-only, by construction
//!
//! This module only ever opens files for reading and shells out to two
//! read-only system tools (`lsof`, `plutil -convert json -o -`). It never
//! executes a hook, a `--version` probe or a `which` lookup, never writes
//! any file, and never collects argv — see
//! `scripts/check-host-dependency-probe-is-read-only.sh`, which asserts
//! this mechanically, and
//! `tests/host_dependency_probe_preserves_host_config.rs`, which asserts
//! it empirically (byte-for-byte + mtime unchanged on every fixture probed).
//!
//! # Injected roots, never ambient `$HOME`
//!
//! [`HostDependencyRoots`] is a required constructor argument of
//! [`LiveHostDependencyProbe`] — there is no code path inside this module
//! that falls back to the real `$HOME`, the real `$PATH`, or a hardcoded
//! `/usr/bin/plutil`. Only [`HostDependencyRoots::from_env`] (called once,
//! at production entrypoints) and `Default for LiveHostDependencyProbe`
//! read the real environment. Every test constructs
//! [`HostDependencyRoots`] by hand, pointed at a fixture directory —
//! reading the developer's own `~/.claude/settings.json` in a test is
//! exactly the HORO-1822 incident's privacy failure, reproduced in CI.
//!
//! # Deliberately out of scope for this ticket (see the HORO-1825 PR body)
//!
//! - **Per-configured-project `claude_project(<root>)` sources** (§4.3):
//!   deferred — wiring the detector-discovered project-root list into
//!   this collector would touch every `DefaultEvidenceCollector::default()`
//!   call site, which is a larger blast radius than this ticket's scope.
//!   Not silently dropped: every executable-bearing kind still gets the
//!   home-scoped sources, and the gap is named in `sources_examined`
//!   never implicitly claiming completeness it doesn't have.
//! - **Codex `~/.codex/hooks.json`**: its schema could not be confirmed
//!   from the installed Codex version available while writing this probe,
//!   so a present file is reported `unresolved` (fail-closed), never
//!   parsed speculatively (§8).
//! - **Codex `~/.codex/config.toml`'s `notify` key**: no already-vendored
//!   dependency can parse TOML (checked: `grep -n '^name = "toml'
//!   Cargo.lock` returns nothing), and adding the `toml` crate for one
//!   field is not justified within this ticket's scope — so a present
//!   file is reported `unresolved` (fail-closed), per §16.3's explicit
//!   permitted fallback.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::open_files::run_lsof;
use super::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::model::{
    DependencyRef, ExeIdentity, ExecutableDependencyReport, ProcessRef, Provenance, SourceTag,
    UnresolvedRef,
};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

/// Hard bound on symlink-chain hops (§4.3). A loop or a chain longer than
/// this is `unresolved`, never a clean negative.
const MAX_SYMLINK_HOPS: usize = 40;

/// Bound on how large a recognized config file may be before it is
/// treated as unreadable rather than parsed. Every real hook/plist config
/// is kilobytes; anything past this is almost certainly not one.
const MAX_CONFIG_FILE_BYTES: u64 = 1_000_000;

/// Bound on how many `~/Library/LaunchAgents/*.plist` files are examined.
const MAX_LAUNCH_AGENTS: usize = 256;

/// Injected host roots. Required to construct [`LiveHostDependencyProbe`]
/// — see the module header for why there is no ambient fallback.
#[derive(Debug, Clone)]
pub struct HostDependencyRoots {
    pub home: PathBuf,
    /// Glomeris's own effective `PATH`, split into directories, in order.
    /// Used ONLY to resolve a bare command name (§4.3 "INFERRED") —
    /// never assumed to match the hook runtime's actual `PATH`.
    pub path_dirs: Vec<PathBuf>,
    /// Path to the `plutil` binary, injectable so a fixture-pointed test
    /// never has to rely on (or fight with) the real one on `PATH`.
    pub plutil_bin: PathBuf,
    /// Path to the `lsof` binary, injectable so a test can force the
    /// running-process probe to fail (e.g. a nonexistent path) without
    /// depending on or fighting with the real one on `PATH`. Defaults to
    /// the bare name `"lsof"`, which `Command` resolves via `PATH` exactly
    /// as before this field existed.
    pub lsof_bin: PathBuf,
    /// Optional managed-settings path (Claude Code's org-managed
    /// settings.json), when the platform defines one. `None` is a normal,
    /// absent-source outcome, not a probe failure.
    pub managed_settings: Option<PathBuf>,
}

impl HostDependencyRoots {
    /// Reads the real environment. Called exactly once, at a production
    /// entrypoint — never from inside a probe's per-call logic.
    pub fn from_env() -> Option<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Some(Self {
            home,
            path_dirs,
            plutil_bin: PathBuf::from("/usr/bin/plutil"),
            lsof_bin: PathBuf::from("lsof"),
            managed_settings: None,
        })
    }
}

/// A single config source this probe knows how to read, paired with the
/// [`SourceTag`] it reports under.
struct ConfigSource {
    tag: SourceTag,
    path: PathBuf,
}

fn recognized_sources(roots: &HostDependencyRoots) -> Vec<ConfigSource> {
    let mut sources = vec![
        ConfigSource {
            tag: SourceTag::ClaudeUserSettings,
            path: roots.home.join(".claude").join("settings.json"),
        },
        ConfigSource {
            tag: SourceTag::ClaudeUserLocal,
            path: roots.home.join(".claude").join("settings.local.json"),
        },
        ConfigSource {
            tag: SourceTag::CodexHooks,
            path: roots.home.join(".codex").join("hooks.json"),
        },
        ConfigSource {
            tag: SourceTag::CodexNotify,
            path: roots.home.join(".codex").join("config.toml"),
        },
    ];
    if let Some(managed) = &roots.managed_settings {
        sources.push(ConfigSource {
            tag: SourceTag::ClaudeManaged,
            path: managed.clone(),
        });
    }
    sources
}

/// Reads a config file's bytes, bounded by [`MAX_CONFIG_FILE_BYTES`].
/// `Ok(None)` means ENOENT (a normal absent source, §4.3/C4) — every other
/// `Err` (unreadable, oversized) is the caller's cue to emit
/// `UnresolvedRef::SourceUnreadable`.
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, ()> {
    match fs::metadata(path) {
        Ok(meta) => {
            if meta.len() > MAX_CONFIG_FILE_BYTES {
                return Err(());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    }
    fs::read(path).map(Some).map_err(|_| ())
}

/// One raw `command`-bearing field, found at a structural JSON pointer,
/// before tokenization.
struct RawCommandRef {
    pointer: String,
    command: String,
}

/// Walks the Claude Code settings schema: `hooks.*[].hooks[].command` and
/// `statusLine.command`.
fn extract_claude_commands(json: &serde_json::Value) -> Vec<RawCommandRef> {
    let mut out = Vec::new();
    if let Some(hooks) = json.get("hooks").and_then(|v| v.as_object()) {
        for (event, entries) in hooks {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for (i, entry) in entries.iter().enumerate() {
                let Some(inner_hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
                    continue;
                };
                for (j, hook) in inner_hooks.iter().enumerate() {
                    if let Some(command) = hook.get("command").and_then(|v| v.as_str()) {
                        out.push(RawCommandRef {
                            pointer: format!("hooks.{event}[{i}].hooks[{j}]"),
                            command: command.to_string(),
                        });
                    }
                }
            }
        }
    }
    if let Some(command) = json
        .get("statusLine")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
    {
        out.push(RawCommandRef {
            pointer: "statusLine.command".to_string(),
            command: command.to_string(),
        });
    }
    out
}

/// A single simple-form token, after tokenization (§4.3). Only `Head` and
/// `PathLike` ever resolve to a [`DependencyRef`] or a path-shaped
/// [`UnresolvedRef`] — `Opaque` (a non-path argument) is not a dependency
/// and is silently ignored, matching the spec's "every argument that is
/// an absolute or ~/ path".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A non-head argument that is absolute or `~/`-prefixed.
    AbsoluteOrHomePath(PathBuf),
    /// A non-head argument that contains `/` but is neither absolute nor
    /// `~/`-prefixed — the independent-review amendment: no defined
    /// resolution cwd, so it is `NonInspectable`, never silently dropped.
    RelativeNonInspectable,
    /// Anything else: not a path, not a dependency, not flagged.
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadToken {
    AbsoluteOrHome(PathBuf),
    BareName(String),
    /// The head itself is a non-absolute, non-`~/` relative path
    /// (contains `/`), which has the same no-defined-cwd problem as a
    /// relative argument.
    RelativeNonInspectable,
}

/// Outcome of tokenizing one raw command string (§4.3).
enum Tokenized {
    Simple {
        head: HeadToken,
        args: Vec<Token>,
    },
    /// Any shell metacharacter, glob, or unbalanced quote => the whole
    /// command is non-inspectable.
    NonInspectable,
}

const SHELL_METACHARS: &[char] = &['$', '`', '|', ';', '&', '>', '<', '(', ')', '*', '?', '['];

/// Tokenizes a command string per §4.3's simple-form rule:
/// `[NAME=VALUE ...] head [arg ...]`. Leading environment assignments are
/// recognized and discarded (their *values* are never stored — only the
/// fact that an assignment was skipped).
fn tokenize_command(raw: &str) -> Tokenized {
    if raw.trim().is_empty() {
        return Tokenized::NonInspectable;
    }
    if raw.chars().any(|c| SHELL_METACHARS.contains(&c)) {
        return Tokenized::NonInspectable;
    }
    // Balanced-quote check: a single or double quote must occur an even
    // number of times, with no escaping — §4.3 only recognizes the
    // simple form, so anything needing real shell-quote semantics is
    // NonInspectable by design, not a parser gap.
    if !raw.matches('\'').count().is_multiple_of(2) || !raw.matches('"').count().is_multiple_of(2) {
        return Tokenized::NonInspectable;
    }
    let mut parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.is_empty() {
        return Tokenized::NonInspectable;
    }
    while let Some(first) = parts.first() {
        if is_env_assignment(first) {
            parts.remove(0);
        } else {
            break;
        }
    }
    let Some((head_raw, rest)) = parts.split_first() else {
        return Tokenized::NonInspectable;
    };
    let head = classify_head(head_raw);
    let args = rest.iter().map(|a| classify_token(a)).collect();
    Tokenized::Simple { head, args }
}

fn is_env_assignment(token: &str) -> bool {
    let Some(eq) = token.find('=') else {
        return false;
    };
    let name = &token[..eq];
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

fn classify_head(raw: &str) -> HeadToken {
    if let Some(path) = as_absolute_or_home(raw) {
        HeadToken::AbsoluteOrHome(path)
    } else if raw.contains('/') {
        HeadToken::RelativeNonInspectable
    } else {
        HeadToken::BareName(raw.to_string())
    }
}

fn classify_token(raw: &str) -> Token {
    if let Some(path) = as_absolute_or_home(raw) {
        Token::AbsoluteOrHomePath(path)
    } else if raw.contains('/') {
        Token::RelativeNonInspectable
    } else {
        Token::Opaque
    }
}

/// Shape check only (absolute, or `~/`-marked for later expansion) —
/// `classify_head`/`classify_token` do not need `~` resolved yet.
/// Resolution against the injected home root happens once, in
/// `expand_home`, where that root is actually in scope.
fn as_absolute_or_home(raw: &str) -> Option<PathBuf> {
    if raw.starts_with("~/") || raw == "~" {
        // Marked, resolved later against the injected home in
        // `resolve_reference`. Using a sentinel keeps tokenization itself
        // free of any home dependency.
        Some(PathBuf::from(raw))
    } else {
        let p = Path::new(raw);
        p.is_absolute().then(|| p.to_path_buf())
    }
}

/// Result of walking a path's symlink chain up to [`MAX_SYMLINK_HOPS`].
enum ChainResolution {
    /// Ended on a real, non-symlink file. `hops` includes every path in
    /// the chain, start to finish.
    Resolved {
        final_path: PathBuf,
        hops: Vec<PathBuf>,
    },
    /// The chain ended on a path that does not exist (a stale symlink, or
    /// the start itself missing).
    Dangling { hops: Vec<PathBuf> },
    /// Looped or exceeded [`MAX_SYMLINK_HOPS`].
    TooDeepOrLoop,
}

/// Walks `start`'s symlink chain. Never canonicalizes the whole path up
/// front (that fails outright on a dangling link, which would make the
/// stale-symlink case silently indistinguishable from "no reference" —
/// see the module header and HORO-1825's AC "stale symlink" case).
fn resolve_symlink_chain(start: &Path) -> ChainResolution {
    let mut hops = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = start.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        if !seen.insert(current.clone()) {
            return ChainResolution::TooDeepOrLoop;
        }
        hops.push(current.clone());
        match fs::symlink_metadata(&current) {
            Err(_) => return ChainResolution::Dangling { hops },
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    let Ok(target) = fs::read_link(&current) else {
                        return ChainResolution::Dangling { hops };
                    };
                    current = if target.is_absolute() {
                        target
                    } else {
                        match current.parent() {
                            Some(parent) => parent.join(target),
                            None => return ChainResolution::Dangling { hops },
                        }
                    };
                } else {
                    return ChainResolution::Resolved {
                        final_path: current.clone(),
                        hops,
                    };
                }
            }
        }
    }
    ChainResolution::TooDeepOrLoop
}

/// Canonicalizes one hop by canonicalizing its PARENT directory and
/// re-appending the file name — never the whole path. Canonicalizing the
/// whole path fails on a dangling symlink and normalizes away macOS
/// tempdir aliasing (`/var` -> `/private/var`) inconsistently hop to hop,
/// which is exactly the trap that would make a stale-symlink or a tempdir
/// fixture resolve wrong. See the module header.
fn canonical_hop(hop: &Path) -> Option<PathBuf> {
    let parent = hop.parent()?;
    let file_name = hop.file_name()?;
    let canon_parent = fs::canonicalize(parent).ok()?;
    Some(canon_parent.join(file_name))
}

/// Result of [`chain_matches_resource`] — tri-state rather than `bool`,
/// because "could not determine" must never collapse into "does not
/// match".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainMatch {
    Yes,
    No,
    /// At least one hop could not be canonicalized — e.g. an intermediate
    /// directory in its path does not exist (a target whose whole parent
    /// chain was never created, not merely the final file). `canonical_hop`
    /// returning `None` for every hop that mattered means this chain's
    /// relationship to the resource genuinely cannot be established, which
    /// is a different fact from "established, and it's outside" — see
    /// `path_matches_resource`'s own doc comment for the matching case
    /// where a MISSING FILE (but resolvable parent) still proves same-
    /// filesystem containment by prefix alone. This variant is for when
    /// even the parent can't be resolved.
    Indeterminate,
}

/// §4.3's match rule: canonical(resolved), or any hop in its chain, is
/// component-wise under the resource's canonical locator, AND same
/// `st_dev`. Returns [`ChainMatch::Indeterminate`] — never a silent `No`
/// — when a hop's containment could not be established at all.
fn chain_matches_resource(
    hops: &[PathBuf],
    resource_canonical: &Path,
    resource_dev: u64,
) -> ChainMatch {
    let mut indeterminate = false;
    for hop in hops {
        match canonical_hop(hop) {
            Some(canon) => {
                if path_matches_resource(&canon, resource_canonical, resource_dev) {
                    return ChainMatch::Yes;
                }
            }
            None => indeterminate = true,
        }
    }
    if indeterminate {
        ChainMatch::Indeterminate
    } else {
        ChainMatch::No
    }
}

/// Shared prefix + same-device check against the resource, given an
/// already-canonicalized candidate path. A candidate that does not exist
/// (ENOENT — the missing-executable and dangling-symlink-target AC cases)
/// cannot be `stat`-confirmed on the resource's device, but its prefix
/// relationship under `resource_canonical` already proves it is on the
/// same filesystem as the resource: a path cannot cross a mount point by
/// merely being a component-wise subpath of an already-resolved
/// directory. Only fall back to requiring a successful `stat` match when
/// the path DOES exist, to still catch a same-named file on a different
/// device reached through some other canonicalization quirk.
fn path_matches_resource(canon: &Path, resource_canonical: &Path, resource_dev: u64) -> bool {
    if !canon.starts_with(resource_canonical) {
        return false;
    }
    match fs::metadata(canon) {
        Ok(meta) => meta.dev() == resource_dev,
        Err(_) => true,
    }
}

fn exe_identity_of(path: &Path) -> Option<ExeIdentity> {
    let meta = fs::metadata(path).ok()?;
    Some(ExeIdentity {
        path: path.to_path_buf(),
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Resolves one raw command string into dependency/unresolved findings
/// against one resource. `source`/`pointer` are carried through for
/// reporting; nothing here stores the raw command text itself in the
/// returned findings.
fn resolve_command(
    raw: &RawCommandRef,
    source: &SourceTag,
    roots: &HostDependencyRoots,
    resource_canonical: &Path,
    resource_dev: u64,
) -> (Vec<DependencyRef>, Vec<UnresolvedRef>) {
    let mut deps = Vec::new();
    let mut unresolved = Vec::new();

    let tokenized = tokenize_command(&raw.command);
    let Tokenized::Simple { head, args } = tokenized else {
        unresolved.push(UnresolvedRef::NonInspectable {
            source: source.clone(),
            pointer: raw.pointer.clone(),
        });
        return (deps, unresolved);
    };

    let mut candidates: Vec<(PathBuf, Provenance)> = Vec::new();
    match head {
        HeadToken::AbsoluteOrHome(p) => {
            candidates.push((expand_home(&p, &roots.home), Provenance::Observed));
        }
        HeadToken::RelativeNonInspectable => {
            unresolved.push(UnresolvedRef::NonInspectable {
                source: source.clone(),
                pointer: raw.pointer.clone(),
            });
        }
        HeadToken::BareName(name) => match resolve_on_path(&name, &roots.path_dirs) {
            // PATH-shadowing amendment (§4.3): a bare-name match found
            // only via Glomeris's own PATH is never a clean negative,
            // regardless of whether that resolution happens to land
            // inside the resource — the hook runtime's actual PATH may
            // diverge either way, so this always stays `unresolved`.
            Some(_resolved) => unresolved.push(UnresolvedRef::PathResolutionDivergent {
                source: source.clone(),
                pointer: raw.pointer.clone(),
            }),
            None => unresolved.push(UnresolvedRef::NotOnPath {
                source: source.clone(),
                pointer: raw.pointer.clone(),
            }),
        },
    }

    for arg in args {
        match arg {
            Token::AbsoluteOrHomePath(p) => {
                candidates.push((expand_home(&p, &roots.home), Provenance::Observed));
            }
            Token::RelativeNonInspectable => {
                unresolved.push(UnresolvedRef::NonInspectable {
                    source: source.clone(),
                    pointer: raw.pointer.clone(),
                });
            }
            Token::Opaque => {}
        }
    }

    for (candidate, provenance) in candidates {
        match resolve_symlink_chain(&candidate) {
            ChainResolution::Resolved { final_path, hops } => {
                match chain_matches_resource(&hops, resource_canonical, resource_dev) {
                    ChainMatch::Yes => {
                        if let Some(exe) = exe_identity_of(&final_path) {
                            deps.push(DependencyRef {
                                source: source.clone(),
                                pointer: raw.pointer.clone(),
                                resolved: final_path,
                                exe,
                                provenance,
                                next_invocation: true,
                            });
                        } else {
                            unresolved.push(UnresolvedRef::SourceUnreadable {
                                source: source.clone(),
                            });
                        }
                    }
                    // A resolved chain that does NOT match the resource is
                    // a clean negative for this candidate — nothing else
                    // to record (§4.3: match is the only thing that
                    // creates either a DependencyRef or an unresolved
                    // finding for an inspectable, fully-resolved path).
                    ChainMatch::No => {}
                    // Containment genuinely could not be established (an
                    // intermediate directory in the chain doesn't exist) —
                    // never read as "outside", which would be a clean
                    // negative built on a gap.
                    ChainMatch::Indeterminate => {
                        unresolved.push(UnresolvedRef::PathResolutionIndeterminate {
                            source: source.clone(),
                            pointer: raw.pointer.clone(),
                        });
                    }
                }
            }
            ChainResolution::Dangling { hops } => {
                // Stale symlink (AC case): if any hop along the way was
                // already inside the resource, that is still a reference
                // — a hook pointing at a now-broken path inside the
                // target is still a dependency the target cannot safely
                // lose. Otherwise, a dangling reference outside the
                // resource is simply not a reference to it.
                match chain_matches_resource(&hops, resource_canonical, resource_dev) {
                    ChainMatch::Yes => {
                        unresolved.push(UnresolvedRef::SymlinkLoopOrTooDeep {
                            source: source.clone(),
                            pointer: raw.pointer.clone(),
                        });
                    }
                    ChainMatch::No => {}
                    // The missing-executable AC case when the WHOLE parent
                    // chain is absent (not merely the final file): no hop
                    // could be canonicalized, so containment could not be
                    // established either way — fail closed, never a
                    // silent "not inside".
                    ChainMatch::Indeterminate => {
                        unresolved.push(UnresolvedRef::PathResolutionIndeterminate {
                            source: source.clone(),
                            pointer: raw.pointer.clone(),
                        });
                    }
                }
            }
            ChainResolution::TooDeepOrLoop => {
                unresolved.push(UnresolvedRef::SymlinkLoopOrTooDeep {
                    source: source.clone(),
                    pointer: raw.pointer.clone(),
                });
            }
        }
    }

    (deps, unresolved)
}

fn is_inside_resource(path: &Path, resource_canonical: &Path, resource_dev: u64) -> bool {
    let Some(canon) = canonical_hop(path) else {
        return false;
    };
    path_matches_resource(&canon, resource_canonical, resource_dev)
}

fn expand_home(marked: &Path, home: &Path) -> PathBuf {
    let s = marked.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else if s == "~" {
        home.to_path_buf()
    } else {
        marked.to_path_buf()
    }
}

/// Resolves a bare command name against `path_dirs`, exactly as a shell's
/// `PATH` lookup would — first match wins, executable-bit not checked
/// (lsof-style "found a file of that name" is all this probe needs; it
/// never executes anything to confirm runnability).
fn resolve_on_path(name: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in path_dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Runs `plutil -convert json -o - <file>` and parses the result. `-o -`
/// is load-bearing: omitting it makes `plutil` rewrite the plist IN
/// PLACE, which this read-only probe must never risk —
/// `scripts/check-host-dependency-probe-is-read-only.sh` asserts the flag
/// is present in this source.
fn read_plist_as_json(
    plutil_bin: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<serde_json::Value, ()> {
    let mut command = Command::new(plutil_bin);
    command
        .arg("-convert")
        .arg("json")
        .arg("-o")
        .arg("-")
        .arg(path);
    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) if output.status.success() => {
            serde_json::from_slice(&output.stdout).map_err(|_| ())
        }
        _ => Err(()),
    }
}

/// Outcome of listing `~/Library/LaunchAgents`. Distinguishes "the
/// directory doesn't exist" (ENOENT — a normal, fully-observed negative:
/// no LaunchAgents configured at all, §4.3/C4) from any OTHER `read_dir`
/// failure (permission denied, I/O error, ...), which must surface as
/// unresolved rather than being silently read as the same clean "no
/// LaunchAgents" outcome.
enum LaunchAgentListing {
    Plists(Vec<PathBuf>),
    DirectoryAbsent,
    Unreadable,
}

fn launch_agent_plists(home: &Path) -> LaunchAgentListing {
    let dir = home.join("Library").join("LaunchAgents");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LaunchAgentListing::DirectoryAbsent;
        }
        Err(_) => return LaunchAgentListing::Unreadable,
    };
    let mut plists: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("plist"))
        .collect();
    plists.sort();
    plists.truncate(MAX_LAUNCH_AGENTS);
    LaunchAgentListing::Plists(plists)
}

fn launch_agent_label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The real, subprocess-backed [`crate::evidence::correlate::EvidenceCollector`]-
/// facing probe. Injected into [`super::DefaultEvidenceCollector`].
pub trait HostDependencyProbe {
    fn probe(
        &self,
        resource_path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<ExecutableDependencyReport>;
}

pub struct LiveHostDependencyProbe {
    roots: HostDependencyRoots,
}

impl LiveHostDependencyProbe {
    pub fn new(roots: HostDependencyRoots) -> Self {
        Self { roots }
    }
}

impl HostDependencyProbe for LiveHostDependencyProbe {
    fn probe(
        &self,
        resource_path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<ExecutableDependencyReport> {
        let Ok(resource_canonical) = fs::canonicalize(resource_path) else {
            // The resource itself doesn't exist/can't be stat'd — this
            // probe has nothing to compare against.
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        };
        let Ok(resource_meta) = fs::metadata(&resource_canonical) else {
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        };
        let resource_dev = resource_meta.dev();

        let mut references_inside = Vec::new();
        let mut unresolved = Vec::new();
        let mut sources_examined = Vec::new();

        for config in recognized_sources(&self.roots) {
            match read_bounded(&config.path) {
                Ok(None) => {
                    // ENOENT: a normal, fully-observed negative (§4.3/C4).
                    sources_examined.push(config.tag.clone());
                }
                Ok(Some(_))
                    if matches!(config.tag, SourceTag::CodexHooks | SourceTag::CodexNotify) =>
                {
                    // Fail-closed deferrals — see module header.
                    sources_examined.push(config.tag.clone());
                    unresolved.push(UnresolvedRef::SourceUnreadable {
                        source: config.tag.clone(),
                    });
                }
                Ok(Some(bytes)) => {
                    sources_examined.push(config.tag.clone());
                    match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(json) => {
                            for raw in extract_claude_commands(&json) {
                                let (mut d, mut u) = resolve_command(
                                    &raw,
                                    &config.tag,
                                    &self.roots,
                                    &resource_canonical,
                                    resource_dev,
                                );
                                references_inside.append(&mut d);
                                unresolved.append(&mut u);
                            }
                        }
                        Err(_) => unresolved.push(UnresolvedRef::SourceUnreadable {
                            source: config.tag.clone(),
                        }),
                    }
                }
                Err(()) => {
                    // Present but unreadable/oversized -> fail closed,
                    // not silently skipped.
                    sources_examined.push(config.tag.clone());
                    unresolved.push(UnresolvedRef::SourceUnreadable {
                        source: config.tag.clone(),
                    });
                }
            }
        }

        let plists = match launch_agent_plists(&self.roots.home) {
            LaunchAgentListing::Plists(plists) => plists,
            // ENOENT: a normal, fully-observed negative.
            LaunchAgentListing::DirectoryAbsent => Vec::new(),
            // Present but unreadable (e.g. EACCES) — fail closed, not a
            // silent "no LaunchAgents".
            LaunchAgentListing::Unreadable => {
                let tag = SourceTag::LaunchAgent("directory".to_string());
                sources_examined.push(tag.clone());
                unresolved.push(UnresolvedRef::SourceUnreadable { source: tag });
                Vec::new()
            }
        };
        for plist in plists {
            let tag = SourceTag::LaunchAgent(launch_agent_label(&plist));
            match read_plist_as_json(&self.roots.plutil_bin, &plist, timeout) {
                Ok(json) => {
                    sources_examined.push(tag.clone());
                    let mut raw_commands = Vec::new();
                    if let Some(program) = json.get("Program").and_then(|v| v.as_str()) {
                        raw_commands.push(RawCommandRef {
                            pointer: "Program".to_string(),
                            command: program.to_string(),
                        });
                    }
                    if let Some(first) = json
                        .get("ProgramArguments")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|v| v.as_str())
                    {
                        raw_commands.push(RawCommandRef {
                            pointer: "ProgramArguments[0]".to_string(),
                            command: first.to_string(),
                        });
                    }
                    for raw in raw_commands {
                        let (mut d, mut u) = resolve_command(
                            &raw,
                            &tag,
                            &self.roots,
                            &resource_canonical,
                            resource_dev,
                        );
                        references_inside.append(&mut d);
                        unresolved.append(&mut u);
                    }
                }
                Err(()) => {
                    sources_examined.push(tag.clone());
                    unresolved.push(UnresolvedRef::SourceUnreadable { source: tag });
                }
            }
        }

        // PATH-entry source: a PATH directory that is itself inside the
        // resource makes the resource a PATH provider (§4.3).
        for dir in &self.roots.path_dirs {
            if is_inside_resource(dir, &resource_canonical, resource_dev) {
                if let Some(exe) = exe_identity_of(dir) {
                    references_inside.push(DependencyRef {
                        source: SourceTag::PathEntry,
                        pointer: "PATH".to_string(),
                        resolved: dir.clone(),
                        exe,
                        provenance: Provenance::Observed,
                        next_invocation: true,
                    });
                } else {
                    // Matches a resource-inside path, but its identity
                    // (dev/ino) couldn't be stat'd — fail closed, same as
                    // `resolve_command`'s handling of the same
                    // `exe_identity_of` failure, never a silent skip.
                    unresolved.push(UnresolvedRef::SourceUnreadable {
                        source: SourceTag::PathEntry,
                    });
                }
            }
        }
        sources_examined.push(SourceTag::PathEntry);

        // The running-process probe (`lsof`) is load-bearing evidence, not
        // an optional extra: a failure here (missing binary, timeout,
        // permission denial) must not be coerced into "nothing is running"
        // — that would be exactly the fail-open ADR-0001 §8 forbids. An
        // `Unavailable` here makes the WHOLE probe `Unavailable`, never a
        // partially-successful `Observed` with an empty `running_inside`.
        let running_inside =
            match running_executables_under(&self.roots.lsof_bin, resource_path, timeout) {
                ProbeOutcome::Observed(procs) => procs,
                ProbeOutcome::Unavailable(reason) => return ProbeOutcome::Unavailable(reason),
            };

        ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside,
            running_inside,
            unresolved,
            sources_examined,
        })
    }
}

/// `lsof -a -d txt +D <path>`: ANDs "executable text segment mapping"
/// with "under this subtree", giving the `Executable` relation (§4.1's
/// `f` field) without widening the shared, already-stable
/// `open_by_process`/`process_cwd_match` probes' output shape across
/// every call site that constructs a [`ProcessRef`] literal — see the
/// HORO-1825 PR body for why this ticket keeps that struct unchanged and
/// adds a dedicated query instead of a `relation` field (left to
/// HORO-1823, which touches `ProcessRef` identity anyway).
fn running_executables_under(
    lsof_bin: &Path,
    path: &Path,
    timeout: Duration,
) -> ProbeOutcome<Vec<ProcessRef>> {
    let mut command = Command::new(lsof_bin);
    command
        .args(["-F", "pcn", "-a", "-d", "txt", "+D"])
        .arg(path);
    run_lsof(command, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-host-dep-{prefix}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn roots_for(home: &Path) -> HostDependencyRoots {
        HostDependencyRoots {
            home: home.to_path_buf(),
            path_dirs: Vec::new(),
            plutil_bin: PathBuf::from("/usr/bin/plutil"),
            lsof_bin: PathBuf::from("lsof"),
            managed_settings: None,
        }
    }

    // ---- tokenizer ----

    #[test]
    fn simple_command_tokenizes_head_and_args() {
        match tokenize_command("/usr/bin/true --flag") {
            Tokenized::Simple { head, .. } => {
                assert_eq!(
                    head,
                    HeadToken::AbsoluteOrHome(PathBuf::from("/usr/bin/true"))
                );
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    #[test]
    fn leading_env_assignment_is_skipped_and_discarded() {
        match tokenize_command("FOO=bar /usr/bin/true") {
            Tokenized::Simple { head, .. } => {
                assert_eq!(
                    head,
                    HeadToken::AbsoluteOrHome(PathBuf::from("/usr/bin/true"))
                );
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    #[test]
    fn dollar_sign_makes_command_non_inspectable() {
        assert!(matches!(
            tokenize_command("echo $HOME"),
            Tokenized::NonInspectable
        ));
    }

    #[test]
    fn pipe_makes_command_non_inspectable() {
        assert!(matches!(
            tokenize_command("cat x | grep y"),
            Tokenized::NonInspectable
        ));
    }

    #[test]
    fn unbalanced_quote_is_non_inspectable() {
        assert!(matches!(
            tokenize_command("echo \"unterminated"),
            Tokenized::NonInspectable
        ));
    }

    #[test]
    fn bare_name_head_classifies_as_bare_name() {
        match tokenize_command("my-hook --flag") {
            Tokenized::Simple { head, .. } => {
                assert_eq!(head, HeadToken::BareName("my-hook".to_string()));
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    /// Independent-review amendment: a relative, non-`~`, non-absolute
    /// path argument (`./target/debug/hook`) must become
    /// `RelativeNonInspectable`, never silently ignored.
    #[test]
    fn relative_path_argument_is_relative_non_inspectable() {
        match tokenize_command("/usr/bin/env ./target/debug/my-hook") {
            Tokenized::Simple { args, .. } => {
                assert!(args.contains(&Token::RelativeNonInspectable));
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    #[test]
    fn relative_head_is_relative_non_inspectable() {
        match tokenize_command("../shared-target/release/hook") {
            Tokenized::Simple { head, .. } => {
                assert_eq!(head, HeadToken::RelativeNonInspectable);
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    #[test]
    fn home_prefixed_argument_is_absolute_or_home() {
        match tokenize_command("/usr/bin/env ~/bin/hook") {
            Tokenized::Simple { args, .. } => {
                assert!(args
                    .iter()
                    .any(|a| matches!(a, Token::AbsoluteOrHomePath(p) if p.to_string_lossy() == "~/bin/hook")));
            }
            Tokenized::NonInspectable => panic!("expected Simple"),
        }
    }

    // ---- symlink chain resolution / matching ----

    #[test]
    fn dependency_inside_resource_is_detected_via_absolute_path() {
        let home = unique_temp_dir("home");
        let resource = unique_temp_dir("target");
        let hook_bin = resource.join("debug").join("hook");
        fs::create_dir_all(hook_bin.parent().unwrap()).unwrap();
        fs::write(&hook_bin, b"#!/bin/sh\n").unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                hook_bin.display()
            ),
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert_eq!(report.references_inside.len(), 1);
        assert_eq!(
            report.references_inside[0].source,
            SourceTag::ClaudeUserSettings
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    #[test]
    fn unreferenced_target_reports_no_dependencies() {
        let home = unique_temp_dir("home2");
        let resource = unique_temp_dir("target2");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"/usr/bin/true"}]}]}}"#,
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report.is_clean());

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// AC case "missing executable": a config references an absolute path
    /// inside the resource that simply does not exist (ENOENT, no symlink
    /// involved at all) — the `Dangling` chain-resolution path with a
    /// single-hop chain. Must not be a clean negative.
    #[test]
    fn missing_executable_referenced_directly_is_unresolved_not_clean() {
        let home = unique_temp_dir("home-missing");
        let resource = unique_temp_dir("target-missing");
        fs::create_dir_all(resource.join("debug")).unwrap();
        let missing = resource.join("debug").join("hook-that-was-deleted");

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                missing.display()
            ),
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            !report.is_clean(),
            "a reference to a missing executable inside the resource must not be a clean negative"
        );
        assert!(report.references_inside.is_empty());
        assert!(!report.unresolved.is_empty());

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Same AC case as above, but with the WHOLE parent chain absent —
    /// not merely the final file. `canonical_hop` then cannot even
    /// canonicalize the hop's parent directory, so the fix-up to
    /// `chain_matches_resource`/`canonical_hop` is what this pins: before
    /// that fix, `chain_matches_resource` read every `canonical_hop`
    /// failure as "not inside" and returned a clean negative here, which
    /// is a different (and worse) bug than the "parent exists, file
    /// doesn't" case above — this is "we cannot tell", not "we checked
    /// and it's outside".
    #[test]
    fn missing_executable_with_absent_parent_chain_is_unresolved_not_clean() {
        let home = unique_temp_dir("home-missing-parent");
        let resource = unique_temp_dir("target-missing-parent");
        fs::create_dir_all(&resource).unwrap();
        // Deliberately NOT created: `release/` itself does not exist, so
        // the hop's parent cannot be canonicalized either.
        let missing = resource.join("release").join("hook-never-built");

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                missing.display()
            ),
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            !report.is_clean(),
            "a reference whose whole parent chain is absent must not be a clean negative \
             just because its containment couldn't be determined"
        );
        assert!(report.references_inside.is_empty());
        assert!(
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::PathResolutionIndeterminate { .. })),
            "expected PathResolutionIndeterminate, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    #[test]
    fn stale_symlink_inside_resource_is_unresolved_not_clean() {
        let home = unique_temp_dir("home3");
        let resource = unique_temp_dir("target3");
        fs::create_dir_all(&resource).unwrap();
        let link = resource.join("hook-link");
        symlink(resource.join("does-not-exist"), &link).unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                link.display()
            ),
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            !report.is_clean(),
            "a stale symlink into the resource must not be a clean negative"
        );
        assert!(report.references_inside.is_empty());
        assert!(!report.unresolved.is_empty());

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    #[test]
    fn non_inspectable_command_is_unresolved() {
        let home = unique_temp_dir("home4");
        let resource = unique_temp_dir("target4");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"sh -c 'echo $HOME'"}]}]}}"#,
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(!report.unresolved.is_empty());
        assert!(matches!(
            report.unresolved[0],
            UnresolvedRef::NonInspectable { .. }
        ));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    #[test]
    fn bare_name_on_path_is_path_resolution_divergent_not_clean_negative() {
        let home = unique_temp_dir("home5");
        let resource = unique_temp_dir("target5");
        let path_dir = unique_temp_dir("pathdir5");
        fs::create_dir_all(&resource).unwrap();
        let hook_bin = path_dir.join("my-hook");
        fs::write(&hook_bin, b"#!/bin/sh\n").unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"my-hook"}]}]}}"#,
        )
        .unwrap();

        let mut roots = roots_for(&home);
        roots.path_dirs = vec![path_dir.clone()];
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        // The amendment: found on Glomeris's PATH is still `unresolved`,
        // never a clean negative — whether or not it happens to point
        // inside the resource.
        assert!(!report.is_clean());
        assert!(report
            .unresolved
            .iter()
            .any(|u| matches!(u, UnresolvedRef::PathResolutionDivergent { .. })));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
        fs::remove_dir_all(&path_dir).ok();
    }

    #[test]
    fn bare_name_not_on_path_is_not_on_path() {
        let home = unique_temp_dir("home6");
        let resource = unique_temp_dir("target6");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"totally-nonexistent-hook-xyz"}]}]}}"#,
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report
            .unresolved
            .iter()
            .any(|u| matches!(u, UnresolvedRef::NotOnPath { .. })));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// False-match path: a hook referencing a file OUTSIDE the resource
    /// that happens to share its basename with something inside the
    /// resource must not match. Matching is by canonical path + dev, not
    /// by name.
    #[test]
    fn false_match_by_name_alone_does_not_match() {
        let home = unique_temp_dir("home7");
        let resource = unique_temp_dir("target7");
        let elsewhere = unique_temp_dir("elsewhere7");
        fs::create_dir_all(resource.join("debug")).unwrap();
        fs::write(resource.join("debug").join("hook"), b"a").unwrap();
        fs::write(elsewhere.join("hook"), b"b").unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                elsewhere.join("hook").display()
            ),
        )
        .unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report.is_clean());

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
        fs::remove_dir_all(&elsewhere).ok();
    }

    /// Codex `config.toml` present (even empty/trivial) must fail closed
    /// to `unresolved`, per the documented interim scope (no `toml` crate
    /// in this PR).
    #[test]
    fn codex_config_toml_present_is_unresolved() {
        let home = unique_temp_dir("home8");
        let resource = unique_temp_dir("target8");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join(".codex").join("config.toml"), b"notify = []\n").unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report
            .unresolved
            .iter()
            .any(|u| matches!(u, UnresolvedRef::SourceUnreadable { source } if *source == SourceTag::CodexNotify)));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Absent native tool (`plutil`): an injectable, wrong `plutil_bin`
    /// must fail closed to `unresolved` for the plist it tried to read,
    /// never a silent clean negative.
    #[test]
    fn absent_plutil_is_unresolved_for_launch_agents() {
        let home = unique_temp_dir("home9");
        let resource = unique_temp_dir("target9");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join("Library").join("LaunchAgents")).unwrap();
        fs::write(
            home.join("Library")
                .join("LaunchAgents")
                .join("com.example.agent.plist"),
            b"not even a real plist",
        )
        .unwrap();

        let mut roots = roots_for(&home);
        roots.plutil_bin = PathBuf::from("/definitely/not/a/real/plutil");
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report
            .unresolved
            .iter()
            .any(|u| matches!(u, UnresolvedRef::SourceUnreadable { source } if matches!(source, SourceTag::LaunchAgent(_)))));

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The fail-open this test guards against (ADR-0001 §8: "lsof
    /// missing, timeout or stderr failure ... => `Unavailable` => Partial
    /// => ASK `EvidenceIncomplete`"): if the running-process probe's
    /// `lsof` invocation cannot run at all, the OVERALL probe must come
    /// back `Unavailable`, never a clean `Observed` report with an empty
    /// `running_inside` — that would silently coerce "lsof failed" into
    /// "nothing is running", exactly the ASK-worthy unknown this probe
    /// must never swallow.
    ///
    /// Injects the failure via `lsof_bin` pointed at a nonexistent path,
    /// which makes `Command::spawn` fail with `NotFound` — the same
    /// injectable-binary seam already used by `plutil_bin` in the
    /// `absent_plutil_is_unresolved_for_launch_agents` test above.
    #[test]
    fn lsof_failure_makes_overall_probe_unavailable_not_clean() {
        let home = unique_temp_dir("home-lsof-fail");
        let resource = unique_temp_dir("target-lsof-fail");
        fs::create_dir_all(&resource).unwrap();

        let mut roots = roots_for(&home);
        roots.lsof_bin = PathBuf::from("/definitely/not/a/real/lsof");
        let probe = LiveHostDependencyProbe::new(roots);
        let outcome = probe.probe(&resource, Duration::from_secs(5));

        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(_)),
            "an lsof failure on the running-process probe must make the whole \
             executable_dependency probe Unavailable, not a clean Observed \
             report with empty running_inside; got {outcome:?}"
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The fail-open this pins: `launch_agent_plists` returning
    /// `Vec::new()` for ANY `read_dir` error (not just ENOENT) made a
    /// permission-denied `~/Library/LaunchAgents` read exactly like "no
    /// LaunchAgents configured" — a clean negative built on a read
    /// failure rather than an observation. `DirectoryAbsent` (the
    /// legitimate case, covered by every other test here that doesn't
    /// create a LaunchAgents directory at all) must stay a clean
    /// negative; only `Unreadable` must surface as `unresolved`.
    #[test]
    fn unreadable_launch_agents_directory_is_unresolved_not_clean() {
        let home = unique_temp_dir("home-la-eacces");
        let resource = unique_temp_dir("target-la-eacces");
        fs::create_dir_all(&resource).unwrap();
        let launch_agents = home.join("Library").join("LaunchAgents");
        fs::create_dir_all(&launch_agents).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let original = fs::metadata(&launch_agents).unwrap().permissions();
        fs::set_permissions(&launch_agents, std::fs::Permissions::from_mode(0o000)).unwrap();

        let roots = roots_for(&home);
        let probe = LiveHostDependencyProbe::new(roots);
        let outcome = probe.probe(&resource, Duration::from_secs(5));

        // Restore permissions before any cleanup/assert path can fail.
        fs::set_permissions(&launch_agents, original).ok();

        // Running as root (some sandboxes) bypasses the permission check
        // entirely, in which case this test cannot exercise the failure
        // path at all — skip rather than assert something environment-
        // dependent.
        let unreadable_by_this_process =
            fs::read_dir(&launch_agents).is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound);
        if unreadable_by_this_process {
            let report = outcome
                .observed()
                .cloned()
                .expect("probe should still observe overall — only the LaunchAgents source failed");
            assert!(
                !report.is_clean(),
                "a permission-denied LaunchAgents directory must not be a clean negative"
            );
            assert!(
                report.unresolved.iter().any(|u| matches!(
                    u,
                    UnresolvedRef::SourceUnreadable {
                        source: SourceTag::LaunchAgent(label)
                    } if label == "directory"
                )),
                "expected SourceUnreadable(LaunchAgent(\"directory\")), got {:?}",
                report.unresolved
            );
        }

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The fail-open this pins: a PATH directory that matches the resource
    /// by prefix (so `is_inside_resource` returns `true`) but whose
    /// identity cannot be `stat`-confirmed (it doesn't actually exist —
    /// a dangling PATH entry) was silently skipped: no `DependencyRef`,
    /// no `UnresolvedRef`, nothing — the same shape of gap
    /// `resolve_command` already closes for its own `exe_identity_of`
    /// call.
    #[test]
    fn path_entry_inside_resource_with_unreadable_identity_is_unresolved_not_silent() {
        let home = unique_temp_dir("home-path-ghost");
        let resource = unique_temp_dir("target-path-ghost");
        fs::create_dir_all(&resource).unwrap();
        // Deliberately not created: matches the resource by canonical
        // prefix, but has no real identity to stat.
        let ghost_path_dir = resource.join("ghost-bin-dir");

        let mut roots = roots_for(&home);
        roots.path_dirs = vec![ghost_path_dir];
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            report.references_inside.is_empty(),
            "a PATH entry with no confirmable identity must never become a DependencyRef"
        );
        assert!(
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SourceUnreadable {
                    source: SourceTag::PathEntry
                }
            )),
            "expected SourceUnreadable(PathEntry), got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }
}
