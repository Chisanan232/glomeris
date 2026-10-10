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

/// Result of walking the Claude Code settings schema: the commands it
/// found, plus the structural pointer of every entry that was PRESENT
/// but did not match the expected shape — never silently skipped as if
/// it simply weren't there.
struct ExtractedCommands {
    commands: Vec<RawCommandRef>,
    /// Structural pointers of malformed-but-present entries.
    malformed: Vec<String>,
}

/// Walks the Claude Code settings schema: `hooks.*[].hooks[].command` and
/// `statusLine.command`. A present `hooks` that isn't an object, a
/// present event list that isn't an array, a present hook entry whose
/// `hooks` field isn't an array, and a present hook with no string
/// `command` are all recorded in `malformed` rather than silently
/// skipped — the same fail-open family as every other probe failure in
/// this module. `statusLine` existing with no `command` key at all is
/// NOT malformed (a legitimate configuration with nothing set there);
/// `statusLine.command` existing but not a string IS.
fn extract_claude_commands(json: &serde_json::Value) -> ExtractedCommands {
    let mut commands = Vec::new();
    let mut malformed = Vec::new();

    if let Some(hooks_val) = json.get("hooks") {
        match hooks_val.as_object() {
            None => malformed.push("hooks".to_string()),
            Some(hooks) => {
                for (event, entries_val) in hooks {
                    let Some(entries) = entries_val.as_array() else {
                        malformed.push(format!("hooks.{event}"));
                        continue;
                    };
                    for (i, entry) in entries.iter().enumerate() {
                        let Some(inner_hooks_val) = entry.get("hooks") else {
                            malformed.push(format!("hooks.{event}[{i}].hooks"));
                            continue;
                        };
                        let Some(inner_hooks) = inner_hooks_val.as_array() else {
                            malformed.push(format!("hooks.{event}[{i}].hooks"));
                            continue;
                        };
                        for (j, hook) in inner_hooks.iter().enumerate() {
                            let pointer = format!("hooks.{event}[{i}].hooks[{j}]");
                            match hook.get("command") {
                                Some(v) => match v.as_str() {
                                    Some(command) => commands.push(RawCommandRef {
                                        pointer,
                                        command: command.to_string(),
                                    }),
                                    None => malformed.push(format!("{pointer}.command")),
                                },
                                None => malformed.push(pointer),
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(status_line) = json.get("statusLine") {
        if let Some(command_val) = status_line.get("command") {
            match command_val.as_str() {
                Some(command) => commands.push(RawCommandRef {
                    pointer: "statusLine.command".to_string(),
                    command: command.to_string(),
                }),
                None => malformed.push("statusLine.command".to_string()),
            }
        }
    }

    ExtractedCommands {
        commands,
        malformed,
    }
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
    /// A bare-name argument that is the TARGET of a recognized exec
    /// wrapper (`env NAME`, `sh -c NAME`, `bash -c NAME`) — subject to
    /// the exact same PATH-divergence check a bare-name HEAD gets, never
    /// silently dropped as `Opaque`.
    WrapperTargetBareName(String),
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
    // Exec-wrapper amendment: `env my-hook`, `/bin/sh -c my-hook` and
    // similar run a bare-name ARGUMENT, not the head — the PATH-shadowing
    // amendment above only inspects the head, so a bare name one argument
    // position to the right was invisible to it. Mark the wrapper's own
    // target argument (if it's a bare name) so the args loop below routes
    // it through the exact same PATH-divergence check as a bare-name
    // head, instead of silently dropping it as an opaque, non-path
    // argument.
    let wrapper_target_idx =
        head_basename(&head).and_then(|basename| wrapper_target_index(basename, rest));
    let args = rest
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let token = classify_token(a);
            if Some(i) == wrapper_target_idx && matches!(token, Token::Opaque) {
                Token::WrapperTargetBareName(a.to_string())
            } else {
                token
            }
        })
        .collect();
    Tokenized::Simple { head, args }
}

/// The basename a wrapper is recognized by: the bare name itself, or the
/// final path component of an absolute/`~/`-marked head. `None` for
/// `RelativeNonInspectable`, which is already flagged `NonInspectable`
/// independently.
fn head_basename(head: &HeadToken) -> Option<&str> {
    match head {
        HeadToken::BareName(name) => Some(name.as_str()),
        HeadToken::AbsoluteOrHome(p) => p.file_name().and_then(|f| f.to_str()),
        HeadToken::RelativeNonInspectable => None,
    }
}

/// Known exec-wrapper basenames and where each one's target argument
/// lives (§4.3 PATH-shadowing amendment, extended to wrapper-passed
/// targets). Closed, deliberately small list — an unrecognized wrapper
/// is not detected, matching this module's closed-recognized-source
/// philosophy rather than guessing at arbitrary wrapper semantics.
///
/// NOT exhaustive, and deliberately not trying to be: round-2 re-review
/// confirmed `nohup`, `setsid`, `xargs`, `/usr/bin/arch`, `timeout`,
/// `nice`, and `caffeinate` still bypass this check via the same
/// `Token::Opaque`-dropped mechanism the original gap used, since their
/// target argument isn't in a fixed position the way `env`'s and
/// `-c`'s are. `zsh -c` is added here because it's macOS's actual
/// default interactive shell in some contexts; the others are a known,
/// documented gap — see the module header and the PR description, never
/// silently claimed closed.
fn wrapper_target_index(head_basename: &str, rest: &[&str]) -> Option<usize> {
    match head_basename {
        // `env [OPTION]... [-] [NAME=VALUE]... [COMMAND [ARG]...]`: skip
        // flags and leading NAME=VALUE assignments; the first remaining
        // token is the real command.
        "env" => rest
            .iter()
            .position(|a| !a.starts_with('-') && !is_env_assignment(a)),
        // `sh -c COMMAND` / `bash -c COMMAND` / `zsh -c COMMAND`: the
        // token immediately after `-c` is the command.
        "sh" | "bash" | "zsh" => rest.iter().position(|a| *a == "-c").and_then(|i| {
            let target = i + 1;
            (target < rest.len()).then_some(target)
        }),
        _ => None,
    }
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

/// Canonicalizes as much of `hop`'s ancestry as actually exists, then
/// re-appends whatever trailing components don't — so a hop several
/// directories deep into a path that was never created (or no longer
/// exists) still produces a definitive, comparable canonical path
/// instead of simply failing.
///
/// A naive "canonicalize the immediate parent" approach only handles the
/// single-level-missing case (the final file absent, its directory
/// present). When the *whole* parent chain is missing — e.g. a stale
/// `$PATH` entry pointing at a directory tree that was never installed,
/// or a hook target several un-built directories deep inside a resource
/// — that approach fails outright, and the caller was previously forced
/// to treat the comparison as unresolvable. That is wrong in both
/// directions: it silently clears genuinely-contained references (the
/// missing chain was inside the resource) and, more commonly, it floods
/// `unresolved` with "indeterminate" findings for ordinary stale PATH
/// entries that are nowhere near the resource and were never ambiguous
/// at all.
///
/// `fs::canonicalize` failing on an ancestor has two structurally
/// different causes that must NOT be handled the same way:
///
/// 1. The ancestor genuinely does not exist as a directory entry at all
///    (ENOENT on a plain, non-symlink path) — safe to climb past: the
///    caller's intent is "this directory was never created", and the
///    nearest REAL ancestor further up is the right place to resume
///    canonicalizing from.
/// 2. The ancestor DOES exist as a directory entry, but it is a symlink
///    whose own target doesn't resolve (dangling, or itself several
///    hops into another dangling chain). Climbing past this as if it
///    were case 1 silently discards the symlink's target entirely —
///    e.g. `~/bin/hooks -> <resource>/release/hooks` where `release/`
///    is simply unbuilt yet: climbing past `~/bin/hooks` up to `~/bin`
///    and reconstructing the literal `~/bin/hooks/...` string never
///    looks at `<resource>/release/hooks/...` at all, so a hook that
///    genuinely depends on the resource (through an on-host symlink
///    pointing straight at it) reads as a clean negative. This is
///    exactly the incident class this probe exists to prevent.
///
/// So on a canonicalize failure, check `symlink_metadata` first: if the
/// ancestor is a symlink, follow it (bounded, loop-guarded) and resume
/// canonicalizing from its target instead of climbing past it; only
/// climb to the parent when the ancestor is genuinely absent.
///
/// Walking upward/through-symlinks until an ancestor canonicalizes (this
/// always terminates — `/` exists, and the hop/loop bound below rules
/// out a symlink cycle) and rebuilding the path from there makes the
/// comparison resolvable in every realistic case. `None` remains
/// possible only for a degenerate input with no file name at all, or a
/// symlink cycle among ancestors.
fn canonical_hop(hop: &Path) -> Option<PathBuf> {
    let file_name = hop.file_name()?;
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor = hop.parent()?.to_path_buf();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_SYMLINK_HOPS {
        if let Ok(canon_ancestor) = fs::canonicalize(&ancestor) {
            let mut result = canon_ancestor;
            for component in missing.iter().rev() {
                result = result.join(component);
            }
            return Some(result.join(file_name));
        }
        match fs::symlink_metadata(&ancestor) {
            // The ancestor exists as a directory entry but is a symlink
            // whose chain doesn't (yet, or ever) resolve. Follow it
            // rather than climbing past it — its target, not the literal
            // on-disk path, is what this ancestor actually refers to.
            Ok(meta) if meta.file_type().is_symlink() => {
                if !seen.insert(ancestor.clone()) {
                    return None; // symlink cycle among ancestors
                }
                let target = fs::read_link(&ancestor).ok()?;
                ancestor = if target.is_absolute() {
                    target
                } else {
                    ancestor.parent()?.join(target)
                };
            }
            // Genuinely absent (ENOENT), or some other stat failure on a
            // non-symlink entry: safe to climb past — nothing here
            // points anywhere else that needs following.
            _ => {
                let name = ancestor.file_name()?.to_os_string();
                missing.push(name);
                ancestor = ancestor.parent()?.to_path_buf();
            }
        }
    }
    None
}

/// Result of [`chain_matches_resource`] — tri-state rather than `bool`,
/// because "could not determine" must never collapse into "does not
/// match".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainMatch {
    Yes,
    No,
    /// `canonical_hop` returned `None` for every hop that mattered — a
    /// degenerate case (no file name component at all) that none of
    /// this module's real candidate paths actually produce, since
    /// `canonical_hop` now climbs to the nearest existing ancestor and
    /// reconstructs the rest rather than failing on a merely-missing
    /// intermediate directory (see its doc comment). Kept as a distinct,
    /// fail-closed variant rather than folded into `No`, so that if a
    /// future change to path construction ever does produce such an
    /// input, it still cannot silently read as "not inside".
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
///
/// Uses `symlink_metadata`, never `metadata`: `canon` is
/// `canonical_hop`'s output, which canonicalizes only the hop's PARENT
/// and re-appends the file name — the final component itself is NOT
/// resolved, so `canon` can legitimately still be a symlink. A hop that
/// is itself a symlink living inside the resource (e.g.
/// `<resource>/link -> /usr/bin/true`) is a reference the resource's
/// deletion would break, regardless of where that symlink points;
/// `fs::metadata` would follow it and stat the TARGET's device instead
/// of the hop's own, turning a same-device, inside-the-resource symlink
/// into a false device mismatch whenever its target happens to live on
/// a different volume (common on macOS's sealed-System-volume layout).
fn path_matches_resource(canon: &Path, resource_canonical: &Path, resource_dev: u64) -> bool {
    if !canon.starts_with(resource_canonical) {
        return false;
    }
    match fs::symlink_metadata(canon) {
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
            // Exec-wrapper amendment: the same PATH-shadowing check a
            // bare-name HEAD gets (§4.3), applied to a wrapper's target
            // argument instead — never a clean negative either way.
            Token::WrapperTargetBareName(name) => match resolve_on_path(&name, &roots.path_dirs) {
                Some(_resolved) => unresolved.push(UnresolvedRef::PathResolutionDivergent {
                    source: source.clone(),
                    pointer: raw.pointer.clone(),
                }),
                None => unresolved.push(UnresolvedRef::NotOnPath {
                    source: source.clone(),
                    pointer: raw.pointer.clone(),
                }),
            },
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
    /// `partially_unreadable` is true when at least one directory ENTRY
    /// (not the whole `read_dir` call — that's `Unreadable` below) failed
    /// to read. A per-entry failure part-way through an iterator (e.g. a
    /// permission-denied or vanished-mid-iteration entry) must not be
    /// silently dropped by a `filter_map(|e| e.ok())`-style skip, the
    /// finer-grained sibling of the whole-directory-failure case.
    Plists {
        plists: Vec<PathBuf>,
        partially_unreadable: bool,
    },
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
    let mut plists: Vec<PathBuf> = Vec::new();
    let mut partially_unreadable = false;
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("plist") {
                    plists.push(path);
                }
            }
            // A per-entry `read_dir` failure (EACCES on one entry,
            // vanished mid-iteration, ...) — never silently dropped as
            // if that entry simply weren't there.
            Err(_) => partially_unreadable = true,
        }
    }
    plists.sort();
    plists.truncate(MAX_LAUNCH_AGENTS);
    LaunchAgentListing::Plists {
        plists,
        partially_unreadable,
    }
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
                            let extracted = extract_claude_commands(&json);
                            for raw in extracted.commands {
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
                            for pointer in extracted.malformed {
                                unresolved.push(UnresolvedRef::MalformedSchema {
                                    source: config.tag.clone(),
                                    pointer,
                                });
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
            LaunchAgentListing::Plists {
                plists,
                partially_unreadable,
            } => {
                // A per-entry `read_dir` failure among otherwise-readable
                // siblings — fail closed on the directory as a whole, same
                // vocabulary as the whole-directory-failure case below,
                // never a silent drop of just that one entry.
                if partially_unreadable {
                    let tag = SourceTag::LaunchAgent("directory".to_string());
                    sources_examined.push(tag.clone());
                    unresolved.push(UnresolvedRef::SourceUnreadable { source: tag });
                }
                plists
            }
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
                    let mut malformed = Vec::new();
                    // `Program` absent entirely is normal — most real
                    // LaunchAgents use `ProgramArguments` instead. Present
                    // but not a string is malformed, never silently
                    // skipped.
                    if let Some(program_val) = json.get("Program") {
                        match program_val.as_str() {
                            Some(program) => raw_commands.push(RawCommandRef {
                                pointer: "Program".to_string(),
                                command: program.to_string(),
                            }),
                            None => malformed.push("Program".to_string()),
                        }
                    }
                    // Same reasoning for `ProgramArguments`: absent
                    // entirely is normal (when `Program` is used instead);
                    // present but not an array, or present with no
                    // string first element, is malformed.
                    if let Some(pa_val) = json.get("ProgramArguments") {
                        match pa_val.as_array() {
                            Some(arr) => match arr.first().and_then(|v| v.as_str()) {
                                Some(first) => raw_commands.push(RawCommandRef {
                                    pointer: "ProgramArguments[0]".to_string(),
                                    command: first.to_string(),
                                }),
                                None => malformed.push("ProgramArguments[0]".to_string()),
                            },
                            None => malformed.push("ProgramArguments".to_string()),
                        }
                    }
                    for pointer in malformed {
                        unresolved.push(UnresolvedRef::MalformedSchema {
                            source: tag.clone(),
                            pointer,
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
        // resource makes the resource a PATH provider (§4.3). Resolves the
        // PATH directory's own FULL symlink chain — never just one hop —
        // exactly like a hook command's candidate path already does via
        // `resolve_symlink_chain` + `chain_matches_resource` in
        // `resolve_command` just above. §4.3 requires "a PATH entry that
        // is itself inside the resource" to be PROTECTED, which includes
        // a PATH entry reached only through an on-host symlink
        // (`~/bin/rt -> <resource>/release`), not merely a PATH string
        // that is itself already a literal subpath of the resource. The
        // prior single-hop `canonical_hop(dir)` check never looked past
        // `dir` at what it points to, so a PATH entry that is a symlink
        // straight at the resource read as a clean negative.
        for dir in &self.roots.path_dirs {
            match resolve_symlink_chain(dir) {
                ChainResolution::Resolved { final_path, hops } => {
                    match chain_matches_resource(&hops, &resource_canonical, resource_dev) {
                        ChainMatch::Yes => {
                            if let Some(exe) = exe_identity_of(&final_path) {
                                references_inside.push(DependencyRef {
                                    source: SourceTag::PathEntry,
                                    pointer: "PATH".to_string(),
                                    resolved: final_path,
                                    exe,
                                    provenance: Provenance::Observed,
                                    next_invocation: true,
                                });
                            } else {
                                // Matches a resource-inside path, but its
                                // identity (dev/ino) couldn't be stat'd —
                                // fail closed, same as `resolve_command`'s
                                // handling of the same `exe_identity_of`
                                // failure, never a silent skip.
                                unresolved.push(UnresolvedRef::SourceUnreadable {
                                    source: SourceTag::PathEntry,
                                });
                            }
                        }
                        ChainMatch::No => {}
                        // The chain's containment could not be established
                        // either way — never read as "outside".
                        ChainMatch::Indeterminate => {
                            unresolved.push(UnresolvedRef::PathResolutionIndeterminate {
                                source: SourceTag::PathEntry,
                                pointer: "PATH".to_string(),
                            });
                        }
                    }
                }
                ChainResolution::Dangling { hops } => {
                    // A PATH entry whose own symlink chain is dangling but
                    // whose chain passed through the resource is still a
                    // dependency (a now-broken on-host symlink that used
                    // to — or is meant to — point into the resource).
                    match chain_matches_resource(&hops, &resource_canonical, resource_dev) {
                        ChainMatch::Yes => {
                            unresolved.push(UnresolvedRef::SymlinkLoopOrTooDeep {
                                source: SourceTag::PathEntry,
                                pointer: "PATH".to_string(),
                            });
                        }
                        ChainMatch::No => {}
                        ChainMatch::Indeterminate => {
                            unresolved.push(UnresolvedRef::PathResolutionIndeterminate {
                                source: SourceTag::PathEntry,
                                pointer: "PATH".to_string(),
                            });
                        }
                    }
                }
                ChainResolution::TooDeepOrLoop => {
                    unresolved.push(UnresolvedRef::SymlinkLoopOrTooDeep {
                        source: SourceTag::PathEntry,
                        pointer: "PATH".to_string(),
                    });
                }
            }
        }
        sources_examined.push(SourceTag::PathEntry);

        // The running-process probe (`lsof`) is load-bearing evidence, not
        // an optional extra: a failure here (missing binary, timeout,
        // permission denial) must not be coerced into "nothing is running"
        // — that would be exactly the fail-open ADR-0001 §8 forbids. It
        // must ALSO never discard `references_inside`/`unresolved` already
        // confirmed by the config/LaunchAgent/PATH-entry probes above —
        // returning a bare `Unavailable` for the whole report on an lsof
        // failure would do exactly that, silently losing a confirmed
        // PROTECTED reference (step 2b reads `executable_dependency.
        // observed()`, which a bare `Unavailable` makes `None`). Recorded
        // as an ordinary `unresolved` finding instead, so "we don't know
        // if something's running" survives alongside whatever else was
        // already found, never silently read as a confirmed empty set.
        sources_examined.push(SourceTag::RunningProcesses);
        let running_inside =
            match running_executables_under(&self.roots.lsof_bin, resource_path, timeout) {
                ProbeOutcome::Observed(procs) => procs,
                ProbeOutcome::Unavailable(_) => {
                    unresolved.push(UnresolvedRef::SourceUnreadable {
                        source: SourceTag::RunningProcesses,
                    });
                    Vec::new()
                }
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
    /// not merely the final file. `canonical_hop` climbs to the nearest
    /// existing ancestor (here, the resource directory itself) and
    /// reconstructs the rest, so `chain_matches_resource` now resolves
    /// this definitively as "inside the resource" (a dangling chain
    /// whose hop matches — same `unresolved`/`SymlinkLoopOrTooDeep`
    /// outcome as the single-missing-file case above, never a clean
    /// negative) instead of "indeterminate". Before the climbing fix,
    /// `canonical_hop` could not canonicalize the hop's parent at all
    /// and `chain_matches_resource` read every such failure as "not
    /// inside" — a clean negative built on an unresolvable comparison.
    #[test]
    fn missing_executable_with_absent_parent_chain_is_unresolved_not_clean() {
        let home = unique_temp_dir("home-missing-parent");
        let resource = unique_temp_dir("target-missing-parent");
        fs::create_dir_all(&resource).unwrap();
        // Deliberately NOT created: `release/` itself does not exist, so
        // the hop's immediate parent cannot be canonicalized — only the
        // resource directory further up can.
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
            "a reference several un-built directories deep inside the resource must not be \
             read as a clean negative just because the intermediate directories don't exist yet"
        );
        assert!(report.references_inside.is_empty());
        // Tightened per round-2 re-review: a vague "some unresolved finding
        // exists" assertion would also pass if the reference were (wrongly)
        // read as `PathResolutionIndeterminate` rather than genuinely
        // matched-but-dangling. Pin the exact variant `canonical_hop`'s
        // climb is meant to produce here: the chain IS resolved to "inside
        // the resource" (climbing to the resource's own real directory and
        // reconstructing the rest), and it's `SymlinkLoopOrTooDeep` —
        // `resolve_command`'s vocabulary for "dangling, but matched" —
        // never `PathResolutionIndeterminate`, which would mean the climb
        // gave up instead of resolving the comparison.
        assert!(
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SymlinkLoopOrTooDeep {
                    source: SourceTag::ClaudeUserSettings,
                    ..
                }
            )),
            "expected SymlinkLoopOrTooDeep(ClaudeUserSettings) — a definitively-matched but \
             dangling chain — got {:?}",
            report.unresolved
        );
        assert!(
            !report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::PathResolutionIndeterminate { .. })),
            "a multi-level-missing chain whose nearest real ancestor is the resource itself \
             must resolve definitively, not fall back to Indeterminate; got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Round-2 re-review MUST-FIX 1's exact repro: a hook command
    /// referencing a path through an on-host symlink whose OWN target is
    /// missing (not merely the final file, and not the hop itself, but
    /// an ancestor of the hop that sits behind a symlink). Before the
    /// fix, `canonical_hop`'s ancestor-climb treated "canonicalize failed
    /// on `~/bin/hooks`" as "this directory doesn't exist, climb past
    /// it" and reconstructed the literal `~/bin/hooks/libra-governor`
    /// string — which has nothing to do with the resource — instead of
    /// following the symlink to see that it points AT
    /// `<resource>/release/hooks`. That produced a clean negative
    /// (`references_inside` empty, `unresolved` empty) for a hook that
    /// genuinely depends on the resource, exactly the incident class
    /// this probe exists to prevent.
    #[test]
    fn symlink_ancestor_with_dangling_target_still_resolves_through_to_the_resource() {
        let home = unique_temp_dir("home-symlink-ancestor");
        let resource = unique_temp_dir("target-symlink-ancestor");
        fs::create_dir_all(&resource).unwrap();
        // Deliberately NOT created: `release/` is unbuilt.
        let release_hooks = resource.join("release").join("hooks");

        fs::create_dir_all(home.join("bin")).unwrap();
        let symlinked_ancestor = home.join("bin").join("hooks");
        // `~/bin/hooks -> <resource>/release/hooks`, and `release/`
        // doesn't exist yet, so the symlink's own target is dangling.
        symlink(&release_hooks, &symlinked_ancestor).unwrap();
        let hook_command = symlinked_ancestor.join("libra-governor");

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
                hook_command.display()
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
            "a hook reached only through an on-host symlink whose target sits inside the \
             resource must not be a clean negative just because the target is currently \
             unbuilt — got references_inside={:?} unresolved={:?}",
            report.references_inside,
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Round-2 re-review MUST-FIX 2's exact repro: a `$PATH` directory
    /// that is ITSELF a symlink straight at the resource (nothing
    /// dangling — the target exists). §4.3 requires this to be PROTECTED
    /// ("a PATH entry that is itself inside the resource"). Before the
    /// fix, the PATH-entry probe only canonicalized a single hop of
    /// `dir` via `canonical_hop` and never followed `dir`'s own symlink
    /// chain the way `resolve_command` already does for hook-command
    /// candidates — so a PATH entry that is a symlink pointing directly
    /// into the resource read as a clean negative.
    #[test]
    fn path_entry_symlinked_straight_at_the_resource_is_protected_not_clean() {
        let home = unique_temp_dir("home-path-symlink");
        let resource = unique_temp_dir("target-path-symlink");
        let release_dir = resource.join("release");
        fs::create_dir_all(&release_dir).unwrap();

        fs::create_dir_all(home.join("bin")).unwrap();
        let symlinked_path_entry = home.join("bin").join("rt");
        // `~/bin/rt -> <resource>/release`, and `release/` exists.
        symlink(&release_dir, &symlinked_path_entry).unwrap();

        let mut roots = roots_for(&home);
        roots.path_dirs = vec![symlinked_path_entry];
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            !report.is_clean(),
            "a PATH entry that is itself a symlink straight into the resource must be \
             recognized as a reference, not a clean negative — got references_inside={:?} \
             unresolved={:?}",
            report.references_inside,
            report.unresolved
        );
        assert!(
            !report.references_inside.is_empty(),
            "the resolved target exists and is genuinely inside the resource — this should \
             be a confirmed DependencyRef, not merely unresolved; got unresolved={:?}",
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
    /// `lsof` invocation cannot run at all, that must surface as an
    /// `unresolved` finding on the report, never be silently coerced into
    /// "nothing is running" (an `Observed` report with a clean empty
    /// `running_inside` and no corresponding `unresolved` entry).
    ///
    /// Injects the failure via `lsof_bin` pointed at a nonexistent path,
    /// which makes `Command::spawn` fail with `NotFound` — the same
    /// injectable-binary seam already used by `plutil_bin` in the
    /// `absent_plutil_is_unresolved_for_launch_agents` test above.
    #[test]
    fn lsof_failure_is_unresolved_not_clean() {
        let home = unique_temp_dir("home-lsof-fail");
        let resource = unique_temp_dir("target-lsof-fail");
        fs::create_dir_all(&resource).unwrap();

        let mut roots = roots_for(&home);
        roots.lsof_bin = PathBuf::from("/definitely/not/a/real/lsof");
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("lsof failing on its own must not make the whole probe Unavailable");

        assert!(report.running_inside.is_empty());
        assert!(
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SourceUnreadable {
                    source: SourceTag::RunningProcesses
                }
            )),
            "expected SourceUnreadable(RunningProcesses), got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The bug the review caught: the ORIGINAL fix for the lsof fail-open
    /// made a failing `lsof` abort the WHOLE probe with a bare
    /// `Unavailable`, which discarded any `references_inside`/`unresolved`
    /// already confirmed by the config/LaunchAgent/PATH-entry probes
    /// earlier in the same call — for the four executable-bearing kinds
    /// that merely downgrades a `PROTECTED` candidate to a consentable
    /// `ASK` (since step 2b reads `executable_dependency.observed()`,
    /// which a bare `Unavailable` makes `None`); for every OTHER
    /// Path-locator kind (not required to have this field complete), a
    /// confirmed hook reference inside a download cache could reach
    /// `AUTO_SAFE` outright if `lsof` merely timed out. A real config
    /// reference combined with a failing `lsof` must survive in
    /// `references_inside` regardless.
    #[test]
    fn confirmed_reference_survives_a_failing_lsof_probe() {
        let home = unique_temp_dir("home-lsof-fail-with-ref");
        let resource = unique_temp_dir("target-lsof-fail-with-ref");
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

        let mut roots = roots_for(&home);
        roots.lsof_bin = PathBuf::from("/definitely/not/a/real/lsof");
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("a failing lsof must not erase an already-confirmed reference");

        assert_eq!(
            report.references_inside.len(),
            1,
            "the confirmed hook reference must survive the lsof probe's own failure, got {:?}",
            report.references_inside
        );
        assert!(
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SourceUnreadable {
                    source: SourceTag::RunningProcesses
                }
            )),
            "the lsof failure must still be recorded, got {:?}",
            report.unresolved
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
    /// by prefix but doesn't actually exist (a dangling PATH entry) was
    /// silently skipped: no `DependencyRef`, no `UnresolvedRef`, nothing.
    /// The PATH-entry probe now resolves the directory's full symlink
    /// chain via `resolve_symlink_chain` (same as a hook command's
    /// candidate path), and a non-existent PATH directory resolves to
    /// `ChainResolution::Dangling` with a single hop; that hop still
    /// matches the resource by prefix, so this surfaces as
    /// `SymlinkLoopOrTooDeep` — the same vocabulary `resolve_command`
    /// already uses for "dangling, but the chain passed through the
    /// resource" — never a silent nothing.
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
                UnresolvedRef::SymlinkLoopOrTooDeep {
                    source: SourceTag::PathEntry,
                    ..
                }
            )),
            "expected SymlinkLoopOrTooDeep(PathEntry), got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Same AC case as above, but with the PATH entry's ENTIRE parent
    /// chain absent (two levels missing, not merely the final
    /// directory). `canonical_hop`'s ancestor-climb still resolves this
    /// to "inside the resource" by reconstructing from the resource's
    /// own (real) directory, so this surfaces the same way as the
    /// single-level case above — never a silent nothing.
    #[test]
    fn path_entry_with_absent_parent_chain_is_unresolved_not_silent() {
        let home = unique_temp_dir("home-path-ghost-deep");
        let resource = unique_temp_dir("target-path-ghost-deep");
        fs::create_dir_all(&resource).unwrap();
        // Neither "missing-parent" nor "ghost-bin-dir" exists.
        let ghost_path_dir = resource.join("missing-parent").join("ghost-bin-dir");

        let mut roots = roots_for(&home);
        roots.path_dirs = vec![ghost_path_dir];
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(report.references_inside.is_empty());
        assert!(
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SymlinkLoopOrTooDeep {
                    source: SourceTag::PathEntry,
                    ..
                }
            )),
            "expected SymlinkLoopOrTooDeep(PathEntry), got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The fail-open this pins: `path_matches_resource` used
    /// `fs::metadata` (follows symlinks) rather than `fs::symlink_metadata`
    /// on a hop that `canonical_hop` leaves unresolved (it canonicalizes
    /// only the PARENT, never the final component). A symlink hop living
    /// INSIDE the resource but pointing to a file on a DIFFERENT device
    /// (routine on macOS's sealed-System-volume layout: `/usr/bin/true` is
    /// commonly on a different volume than a `/private/tmp` fixture) was
    /// stat'd through to its target's device, failing the same-device
    /// check and producing a false `ChainMatch::No` for a hop that is, by
    /// its own location, genuinely a reference inside the resource.
    #[test]
    fn symlink_hop_inside_resource_matches_regardless_of_cross_device_target() {
        let home = unique_temp_dir("home-cross-device");
        let resource = unique_temp_dir("target-cross-device");
        fs::create_dir_all(&resource).unwrap();

        let external_target = Path::new("/usr/bin/true");
        if !external_target.exists() {
            // Not present on this machine — nothing to point the symlink
            // at; skip rather than fail on an environment this test
            // cannot exercise.
            fs::remove_dir_all(&home).ok();
            fs::remove_dir_all(&resource).ok();
            return;
        }
        let resource_dev = fs::metadata(&resource).unwrap().dev();
        let target_dev = fs::metadata(external_target).unwrap().dev();
        if resource_dev == target_dev {
            // This machine's tempdir and /usr/bin happen to share a
            // device (e.g. a single-volume layout) — the bug this test
            // guards against cannot manifest here. Skip rather than
            // assert something environment-dependent.
            fs::remove_dir_all(&home).ok();
            fs::remove_dir_all(&resource).ok();
            return;
        }

        let link = resource.join("link-to-external");
        symlink(external_target, &link).unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
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

        assert_eq!(
            report.references_inside.len(),
            1,
            "a symlink hop living inside the resource must match regardless of its \
             cross-device target; unresolved: {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// The should-address #4 fix: `env my-hook` runs `my-hook`, not
    /// `env` — before this fix, the bare name one argument to the right
    /// of the head was classified `Opaque` and silently dropped, never
    /// reaching the PATH-divergence check at all. `my-hook` resolves on
    /// Glomeris's own PATH here, so the amendment's rule applies:
    /// `unresolved`, never a clean negative.
    #[test]
    fn env_wrapper_target_bare_name_is_path_resolution_divergent() {
        let home = unique_temp_dir("home-env-wrapper");
        let resource = unique_temp_dir("target-env-wrapper");
        let path_dir = unique_temp_dir("pathdir-env-wrapper");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(&path_dir).unwrap();
        fs::write(path_dir.join("my-hook"), b"#!/bin/sh\n").unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"/usr/bin/env my-hook"}]}]}}"#,
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

        assert!(
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::PathResolutionDivergent { .. })),
            "env's wrapped bare-name target must reach the PATH-divergence check, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
        fs::remove_dir_all(&path_dir).ok();
    }

    /// Same amendment, for `sh -c my-hook` / `bash -c my-hook`: the token
    /// immediately after `-c` is the real command, not an opaque flag
    /// argument to `sh`/`bash` themselves.
    #[test]
    fn shell_dash_c_wrapper_target_bare_name_is_not_on_path() {
        let home = unique_temp_dir("home-sh-wrapper");
        let resource = unique_temp_dir("target-sh-wrapper");
        fs::create_dir_all(&resource).unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"/bin/sh -c totally-nonexistent-wrapped-hook-xyz"}]}]}}"#,
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
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::NotOnPath { .. })),
            "sh -c's wrapped bare-name target must reach the PATH-divergence check, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Round-2 re-review should-address #4: `zsh -c` must get the same
    /// treatment as `sh -c`/`bash -c` — it's macOS's actual default
    /// interactive shell in some contexts, so excluding it would leave a
    /// common real-world case unprotected.
    #[test]
    fn zsh_dash_c_wrapper_target_bare_name_is_not_on_path() {
        let home = unique_temp_dir("home-zsh-wrapper");
        let resource = unique_temp_dir("target-zsh-wrapper");
        fs::create_dir_all(&resource).unwrap();

        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"zsh -c totally-nonexistent-wrapped-hook-xyz"}]}]}}"#,
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
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::NotOnPath { .. })),
            "zsh -c's wrapped bare-name target must reach the PATH-divergence check, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Should-address #6: a hook entry whose `command` isn't a string
    /// must not be silently skipped as if the entry didn't exist.
    #[test]
    fn hook_entry_with_non_string_command_is_malformed_schema() {
        let home = unique_temp_dir("home-malformed-command");
        let resource = unique_temp_dir("target-malformed-command");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":42}]}]}}"#,
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
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::MalformedSchema { .. })),
            "a non-string command must be flagged, not silently skipped, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Should-address #6: a hook entry missing the `command` key entirely
    /// is the same fail-open shape — zero findings before this fix.
    #[test]
    fn hook_entry_with_missing_command_key_is_malformed_schema() {
        let home = unique_temp_dir("home-missing-command-key");
        let resource = unique_temp_dir("target-missing-command-key");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":[{"hooks":[{"timeout":5}]}]}}"#,
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
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::MalformedSchema { .. })),
            "a hook entry with no command key must be flagged, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Should-address #6: `hooks.<event>` present but not an array.
    #[test]
    fn hooks_event_value_not_an_array_is_malformed_schema() {
        let home = unique_temp_dir("home-malformed-event");
        let resource = unique_temp_dir("target-malformed-event");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            r#"{"hooks":{"PostToolUse":"not-an-array"}}"#,
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
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::MalformedSchema { .. })),
            "expected MalformedSchema, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Should-address #6, LaunchAgent half: `ProgramArguments` present
    /// but not an array must be flagged, not silently skipped.
    #[test]
    fn launch_agent_program_arguments_not_an_array_is_malformed_schema() {
        let home = unique_temp_dir("home-la-malformed");
        let resource = unique_temp_dir("target-la-malformed");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join("Library").join("LaunchAgents")).unwrap();
        fs::write(
            home.join("Library")
                .join("LaunchAgents")
                .join("com.example.malformed.plist"),
            r#"{"Label":"com.example.malformed","ProgramArguments":"not-an-array"}"#,
        )
        .unwrap();

        // `plutil_bin` here needs to actually convert — reuse `/bin/cat`
        // is not valid plutil behaviour, so point at the real `plutil`
        // (this fixture file is already valid JSON, which `plutil
        // -convert json -o -` passes through unchanged).
        let mut roots = roots_for(&home);
        roots.plutil_bin = PathBuf::from("/usr/bin/plutil");
        let probe = LiveHostDependencyProbe::new(roots);
        let report = probe
            .probe(&resource, Duration::from_secs(5))
            .observed()
            .cloned()
            .expect("probe should observe");

        assert!(
            report
                .unresolved
                .iter()
                .any(|u| matches!(u, UnresolvedRef::MalformedSchema { .. })),
            "expected MalformedSchema for a non-array ProgramArguments, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }

    /// Item 5 follow-up (test-coverage gap the review named explicitly):
    /// the real `serde_json` parse-error branch for a non-Codex config,
    /// exercised end to end rather than only asserted indirectly.
    #[test]
    fn unparseable_claude_settings_json_is_source_unreadable() {
        let home = unique_temp_dir("home-bad-json");
        let resource = unique_temp_dir("target-bad-json");
        fs::create_dir_all(&resource).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(
            home.join(".claude").join("settings.json"),
            b"{ this is not valid json .. ",
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
            report.unresolved.iter().any(|u| matches!(
                u,
                UnresolvedRef::SourceUnreadable {
                    source: SourceTag::ClaudeUserSettings
                }
            )),
            "expected SourceUnreadable(ClaudeUserSettings) from the serde_json parse-error \
             branch, got {:?}",
            report.unresolved
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&resource).ok();
    }
}
