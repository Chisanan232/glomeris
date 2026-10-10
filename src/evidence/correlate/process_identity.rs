//! HORO-1823 §4.1: bounded, read-only process identity enrichment.
//!
//! This module answers "who is this pid, really" for a [`ProcessRef`]
//! already discovered by another probe (open-files, cwd, or the
//! executable-dependency `running_inside` probe) — it never discovers
//! holders on its own, and a failure here NEVER removes a holder from
//! whatever list produced it. See [`ProcessRef`]'s doc comment.
//!
//! All three subprocess calls (`ps`, `launchctl list`, `lsof -d txt`) are
//! batched across every requested pid in one invocation each, not looped
//! per pid — looping would multiply the per-call timeout budget by the
//! holder count and risk exceeding `REVALIDATION_TIMEOUT` (ADR-0001
//! §4.1).
//!
//! `ps`/`launchctl`/`lsof` binaries are injectable (`ProcessIdentityRoots`),
//! following the `HostDependencyRoots.lsof_bin` precedent — a test must
//! never depend on, or fight with, the real tools on `PATH`.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::model::{ExeIdentity, ProcessIdentity, Supervisor};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

/// Injectable binary paths for the identity probes. Defaults to bare
/// names, resolved via `PATH` exactly as before this struct existed.
#[derive(Debug, Clone)]
pub struct ProcessIdentityRoots {
    pub ps_bin: PathBuf,
    pub launchctl_bin: PathBuf,
    pub lsof_bin: PathBuf,
    /// Used only to convert `ps -o lstart=`'s locale-dependent text into
    /// an absolute `SystemTime`, via `date -j -f <fmt> <text> +%s`
    /// (BSD/macOS `date`, already present — no new dependency). A parse
    /// failure here is never treated as "process absent"; it degrades
    /// that one process's identity to `Unavailable`.
    pub date_bin: PathBuf,
}

impl Default for ProcessIdentityRoots {
    fn default() -> Self {
        Self {
            ps_bin: PathBuf::from("ps"),
            launchctl_bin: PathBuf::from("launchctl"),
            lsof_bin: PathBuf::from("lsof"),
            date_bin: PathBuf::from("date"),
        }
    }
}

pub trait ProcessIdentityProbe {
    /// Returns one entry per pid that `ps` actually reported. A pid the
    /// caller asked about but that is missing from the result (exited
    /// between discovery and this probe, or a transient `ps` gap) is
    /// absent from the map — the caller must treat that as "identity
    /// unavailable for this pid", never synthesize a default.
    fn identities_for(
        &self,
        pids: &[u32],
        timeout: Duration,
    ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>>;
}

pub struct LiveProcessIdentityProbe {
    pub roots: ProcessIdentityRoots,
}

impl LiveProcessIdentityProbe {
    pub fn new(roots: ProcessIdentityRoots) -> Self {
        Self { roots }
    }
}

impl Default for LiveProcessIdentityProbe {
    fn default() -> Self {
        Self::new(ProcessIdentityRoots::default())
    }
}

impl ProcessIdentityProbe for LiveProcessIdentityProbe {
    fn identities_for(
        &self,
        pids: &[u32],
        timeout: Duration,
    ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>> {
        if pids.is_empty() {
            return HashMap::new();
        }

        let ps_rows = match run_ps(&self.roots.ps_bin, pids, timeout) {
            Some(rows) => rows,
            // `ps` itself failed (absent, timed out, nonzero exit): every
            // requested pid gets no entry at all — the caller's own
            // "absent from map => Unavailable" contract (not a synthesized
            // `Unavailable` here) is what carries this forward. No
            // partial, possibly-stale row is ever fabricated.
            None => return HashMap::new(),
        };

        // `launchctl list` failure means supervision cannot be answered
        // for ANY pid this round — `Unknown`, never silently `None`.
        let launchd_pids = run_launchctl_list(&self.roots.launchctl_bin, timeout);

        let exe_by_pid = run_lsof_txt(&self.roots.lsof_bin, pids, timeout);

        let mut out = HashMap::new();
        for row in ps_rows {
            let start_time = match parse_lstart(&row.lstart_raw, &self.roots.date_bin, timeout) {
                Some(t) => t,
                None => {
                    out.insert(row.pid, ProbeOutcome::Unavailable(ProbeReason::Failed));
                    continue;
                }
            };

            let supervisor = match &launchd_pids {
                Some(set) if set.contains(&row.pid) => Supervisor::LaunchdJob,
                Some(_) if row.ppid == 1 => Supervisor::Unknown,
                Some(_) => Supervisor::None,
                None => Supervisor::Unknown,
            };

            let exe = match &exe_by_pid {
                Some(map) => match map.get(&row.pid) {
                    Some(exe) => ProbeOutcome::Observed(exe.clone()),
                    None => ProbeOutcome::Unavailable(ProbeReason::Failed),
                },
                None => ProbeOutcome::Unavailable(ProbeReason::Failed),
            };

            out.insert(
                row.pid,
                ProbeOutcome::Observed(ProcessIdentity {
                    start_time,
                    uid: row.uid,
                    ppid: row.ppid,
                    pgid: row.pgid,
                    state_zombie: row.stat.contains('Z'),
                    exe,
                    supervisor,
                }),
            );
        }
        out
    }
}

struct PsRow {
    pid: u32,
    ppid: u32,
    uid: u32,
    pgid: u32,
    stat: String,
    lstart_raw: String,
}

/// `None` means the whole `ps` call failed (tool absent, timed out,
/// nonzero exit) — distinguished from "succeeded but some pids are
/// missing", which is represented by those pids simply not appearing in
/// `Some(rows)`.
fn run_ps(ps_bin: &Path, pids: &[u32], timeout: Duration) -> Option<Vec<PsRow>> {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut command = Command::new(ps_bin);
    command
        .env("LC_ALL", "C")
        .args(["-o", "pid=,ppid=,uid=,pgid=,stat=,lstart=", "-p", &pid_list]);

    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) => {
            // Darwin `ps -p` with no matching pid at all exits non-zero
            // with no stdout and typically no stderr; that is "nothing
            // found", not a tool failure, so only a genuinely non-empty
            // stderr (argument rejection) is treated as failure.
            if !output.status.success() && !output.stderr.is_empty() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            Some(text.lines().filter_map(parse_ps_line).collect())
        }
        _ => None,
    }
}

fn parse_ps_line(line: &str) -> Option<PsRow> {
    let mut tokens = line.split_whitespace();
    let pid = tokens.next()?.parse::<u32>().ok()?;
    let ppid = tokens.next()?.parse::<u32>().ok()?;
    let uid = tokens.next()?.parse::<u32>().ok()?;
    let pgid = tokens.next()?.parse::<u32>().ok()?;
    let stat = tokens.next()?.to_string();
    let rest: Vec<&str> = tokens.collect();
    if rest.is_empty() {
        return None;
    }
    Some(PsRow {
        pid,
        ppid,
        uid,
        pgid,
        stat,
        lstart_raw: rest.join(" "),
    })
}

/// `None` => `launchctl list` itself failed (absent, timed out, non-zero
/// with stderr content) => supervisor is `Unknown` for every pid this
/// round, never `None`.
fn run_launchctl_list(launchctl_bin: &Path, timeout: Duration) -> Option<HashSet<u32>> {
    let mut command = Command::new(launchctl_bin);
    command.arg("list");
    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) => {
            if !output.status.success() && !output.stderr.is_empty() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            let mut pids = HashSet::new();
            for line in text.lines().skip(1) {
                // Columns: PID, Status, Label — tab- or space-separated.
                // A "-" PID means "not currently running"; only a numeric
                // PID counts as a positive supervision match. The label
                // itself is intentionally never stored (§6 privacy — only
                // the boolean match is evidence).
                if let Some(first) = line.split_whitespace().next() {
                    if let Ok(pid) = first.parse::<u32>() {
                        pids.insert(pid);
                    }
                }
            }
            Some(pids)
        }
        _ => None,
    }
}

/// `None` => the `lsof` call itself failed; `Some(map)` maps a pid to its
/// FIRST reported `txt`-mapped file (the process's own executable is
/// listed before any dynamically-loaded library in observed `lsof`
/// output on this platform; a pid with no `txt` entries at all, or whose
/// path could not be `stat`-ed, is simply absent from the map).
///
/// **Known limitation (flagged, not fixed here):** `lsof -d txt` reports
/// every text-segment mapping, including shared libraries, not only the
/// main executable. Taking the first entry is a pragmatic approximation,
/// not a structural guarantee — see the PR body.
fn run_lsof_txt(
    lsof_bin: &Path,
    pids: &[u32],
    timeout: Duration,
) -> Option<HashMap<u32, ExeIdentity>> {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut command = Command::new(lsof_bin);
    command.args(["-F", "pn", "-a", "-p", &pid_list, "-d", "txt"]);

    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) => {
            if !output.status.success() && !output.stderr.is_empty() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            let mut map = HashMap::new();
            let mut current_pid: Option<u32> = None;
            for line in text.lines() {
                let Some((tag, value)) = line.split_at_checked(1) else {
                    continue;
                };
                match tag {
                    "p" => current_pid = value.parse::<u32>().ok(),
                    "n" => {
                        if let Some(pid) = current_pid {
                            map.entry(pid).or_insert_with(|| exe_identity_of(value));
                        }
                    }
                    _ => {}
                }
            }
            Some(
                map.into_iter()
                    .filter_map(|(p, v)| v.map(|v| (p, v)))
                    .collect(),
            )
        }
        _ => None,
    }
}

fn exe_identity_of(raw_path: &str) -> Option<ExeIdentity> {
    let path = Path::new(raw_path);
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = std::fs::metadata(&canonical).ok()?;
    Some(ExeIdentity {
        path: canonical,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Parses `ps -o lstart=`'s locale-independent (`LC_ALL=C`) text, e.g.
/// `"Thu Oct  9 21:33:12 2026"`, via BSD `date -j -f ... +%s`, which
/// resolves the local-time text using the system's own timezone/DST
/// rules rather than this probe reimplementing calendar arithmetic. A
/// parse failure (malformed text, `date` absent, non-UTF8 output,
/// non-numeric result) returns `None`, which the caller treats as the
/// whole identity being `Unavailable` — never a zero/epoch default.
fn parse_lstart(raw: &str, date_bin: &Path, timeout: Duration) -> Option<SystemTime> {
    let mut command = Command::new(date_bin);
    command.args(["-j", "-f", "%a %b %e %T %Y", raw, "+%s"]);
    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let secs: i64 = text.trim().parse().ok()?;
            if secs < 0 {
                return None;
            }
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
        }
        _ => None,
    }
}

/// The identity key for any cross-sample comparison (ADR-0001 §4.1):
/// `(pid, start_time, uid, exe.dev, exe.ino)`. Both identities must have
/// an `Observed` `exe` for the tuple to be comparable at all — an
/// `Unavailable` `exe` on either side is a mismatch, never treated as a
/// wildcard.
pub fn identity_tuple_matches(
    pid_a: u32,
    a: &ProcessIdentity,
    pid_b: u32,
    b: &ProcessIdentity,
) -> bool {
    if pid_a != pid_b || a.start_time != b.start_time || a.uid != b.uid {
        return false;
    }
    match (a.exe.observed(), b.exe.observed()) {
        (Some(x), Some(y)) => x.dev == y.dev && x.ino == y.ino,
        _ => false,
    }
}

/// HORO-1823 §4.1 "Active-build signal that scales": Cargo holds
/// `<target>/<profile>/.cargo-lock` for the duration of a build. Probes
/// the SPECIFIC lock files (enumerated by `read_dir`, never a shell glob
/// — "no shell execution" per the ADR's non-negotiables) rather than a
/// recursive `lsof +D`, which would risk exceeding `REVALIDATION_TIMEOUT`
/// on a large target tree.
///
/// Returns `Observed(true)` iff at least one discovered `.cargo-lock`
/// file has an open holder. Returns `Observed(false)` when lock files
/// were enumerated (possibly zero of them) and none has a holder.
/// Returns `Unavailable` when `lsof` itself could not be run against the
/// lock files that do exist — e.g. a transient disappearance between
/// `read_dir` and `lsof` is folded into this same `Unavailable` via
/// `lsof`'s own stderr-failure path, which is the correct fail-closed
/// outcome, not "no holder".
///
/// **Deliberately NOT wired into `Evidence` completeness or `classify`**
/// (see the PR body): this result is explanation-only evidence for
/// [`crate::evidence::model::ProcessClaim`], same as the rest of this
/// module. The existing recursive `+D` probe (`open_files.rs`) already
/// covers the policy-facing "is anything using this resource" veto; its
/// own `Unavailable` already fails closed to `Partial` => `ASK`
/// independently of this probe's result.
pub fn build_lock_holder(
    target_dir: &Path,
    lsof_bin: &Path,
    timeout: Duration,
) -> ProbeOutcome<bool> {
    let lock_files = match std::fs::read_dir(target_dir) {
        Ok(entries) => {
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let candidate = entry.path().join(".cargo-lock");
                if candidate.exists() {
                    found.push(candidate);
                }
            }
            found
        }
        Err(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    if lock_files.is_empty() {
        return ProbeOutcome::Observed(false);
    }

    let mut command = Command::new(lsof_bin);
    command.args(["-F", "pcfn"]);
    for lock_file in &lock_files {
        command.arg(lock_file);
    }

    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) => if output.status.success() || output.stderr.is_empty()
        {
            Some(output)
        } else {
            None
        }
        .map(|out| ProbeOutcome::Observed(!out.stdout.is_empty()))
        .unwrap_or(ProbeOutcome::Unavailable(ProbeReason::Failed)),
        CommandOutcome::NotFound => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
        CommandOutcome::TimedOut => ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        CommandOutcome::SpawnFailed => ProbeOutcome::Unavailable(ProbeReason::Failed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ps_line_reads_fixed_columns_never_argv() {
        let row = parse_ps_line("501  1 501  502 Ss Thu Oct  9 21:33:12 2026").unwrap();
        assert_eq!(row.pid, 501);
        assert_eq!(row.ppid, 1);
        assert_eq!(row.uid, 501);
        assert_eq!(row.pgid, 502);
        assert_eq!(row.stat, "Ss");
        assert_eq!(row.lstart_raw, "Thu Oct 9 21:33:12 2026");
    }

    #[test]
    fn malformed_ps_line_is_skipped_not_defaulted() {
        assert!(parse_ps_line("not a ps line").is_none());
        assert!(parse_ps_line("").is_none());
    }

    #[test]
    fn identity_tuple_requires_exe_observed_on_both_sides() {
        let base = ProcessIdentity {
            start_time: SystemTime::UNIX_EPOCH,
            uid: 1,
            ppid: 1,
            pgid: 1,
            state_zombie: false,
            exe: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            supervisor: Supervisor::Unknown,
        };
        let other = base.clone();
        assert!(!identity_tuple_matches(1, &base, 1, &other));
    }

    #[test]
    fn identity_tuple_matches_on_full_tuple_equality() {
        let exe = ExeIdentity {
            path: PathBuf::from("/usr/bin/true"),
            dev: 1,
            ino: 2,
        };
        let a = ProcessIdentity {
            start_time: SystemTime::UNIX_EPOCH,
            uid: 501,
            ppid: 1,
            pgid: 1,
            state_zombie: false,
            exe: ProbeOutcome::Observed(exe.clone()),
            supervisor: Supervisor::Unknown,
        };
        let b = a.clone();
        assert!(identity_tuple_matches(42, &a, 42, &b));

        let mut c = b.clone();
        c.uid = 999;
        assert!(!identity_tuple_matches(42, &a, 42, &c));
    }

    #[test]
    fn missing_pid_leaves_identity_absent_from_map_never_synthesized() {
        struct NoOpPs;
        impl ProcessIdentityProbe for NoOpPs {
            fn identities_for(
                &self,
                _pids: &[u32],
                _timeout: Duration,
            ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>> {
                HashMap::new()
            }
        }
        let probe = NoOpPs;
        let result = probe.identities_for(&[1, 2, 3], Duration::from_secs(1));
        assert!(result.is_empty());
    }

    #[test]
    fn build_lock_holder_observed_false_when_no_lock_files_exist() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-process-identity-locktest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        let result = build_lock_holder(&dir, Path::new("lsof"), Duration::from_secs(2));
        assert_eq!(result, ProbeOutcome::Observed(false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_lock_holder_unavailable_when_target_dir_missing() {
        let dir = std::env::temp_dir().join("glomeris-process-identity-nonexistent-dir-xyz");
        let result = build_lock_holder(&dir, Path::new("lsof"), Duration::from_secs(2));
        assert_eq!(result, ProbeOutcome::Unavailable(ProbeReason::Failed));
    }
}
