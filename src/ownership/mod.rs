//! Managed generated-resource authority. Existing resources are never implicitly enrolled.
//! Live holders retain shared kernel barriers; retirement uses the same barrier exclusively.
mod access;
#[cfg(test)]
pub(crate) mod crash_tests;
pub mod native;

use crate::evidence::correlate::timeout::{
    run_with_timeout, run_with_timeout_retrying, CommandOutcome,
};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Lifecycle {
    Active,
    Idle,
    Stalled,
    Orphaned,
    Unknown,
    Reclaimable,
    Reclaimed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassPolicy {
    pub idle_threshold_secs: u64,
    pub unattended: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPolicy {
    pub schema: u32,
    pub classes: BTreeMap<String, ClassPolicy>,
    pub max_actions: usize,
    pub max_bytes: u64,
}
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            schema: 1,
            classes: BTreeMap::from([(
                "managed_generated".into(),
                ClassPolicy {
                    idle_threshold_secs: 86400,
                    unattended: false,
                },
            )]),
            max_actions: 3,
            max_bytes: 5 * 1024 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub pid: u32,
    pub start: String,
    pub executable: String,
    pub boot: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub token: String,
    pub host: String,
    pub session: String,
    pub holder: Identity,
    pub owner: Identity,
    pub lease_until: u64,
    pub progress_sequence: u64,
    pub children: Vec<Identity>,
    pub pending_children: u32,
    pub released: bool,
    pub health_faults: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerProof {
    pub input: PathBuf,
    pub input_metadata: Artifact,
    pub compiler_version: String,
    pub outputs: BTreeMap<String, Artifact>,
    pub closed_consumers: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraversalCursor {
    schema: u32,
    last_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementAttempt {
    pub attempt_id: String,
    pub generation: String,
    pub dev: u64,
    pub ino: u64,
    pub intent_at: u64,
    pub retry_queued_at: Option<u64>,
    pub completed_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadClaim {
    pub token: String,
    pub holder: Identity,
    pub artifact: String,
    pub ended: bool,
    pub exported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub id: String,
    pub generation: String,
    pub class: String,
    pub recipe: String,
    pub source_root: PathBuf,
    pub source_head: String,
    pub dev: u64,
    pub ino: u64,
    pub last_meaningful_use: u64,
    pub claims: Vec<Claim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_claims: Vec<ReadClaim>,
    pub sealed: Option<BTreeMap<String, Artifact>>,
    pub producer: Option<ProducerProof>,
    pub production_started: bool,
    pub retirement_started: bool,
    #[serde(default)]
    pub retirement_history: Vec<RetirementAttempt>,
    pub reclaimed: bool,
    pub observed_free_delta: Option<i64>,
}
#[derive(Debug, Serialize)]
pub struct Inspection {
    pub id: String,
    pub generation: String,
    pub path: PathBuf,
    pub lifecycle: Lifecycle,
    pub eligible: bool,
    pub blockers: Vec<String>,
    pub last_meaningful_use: u64,
    pub inactivity_eligible: bool,
}
#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}
fn error(s: impl std::fmt::Display) -> io::Error {
    io::Error::other(s.to_string())
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn random() -> io::Result<String> {
    let mut b = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
fn valid_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
fn secure(path: &Path, directory: bool) -> io::Result<()> {
    for ancestor in path.ancestors() {
        let m = fs::symlink_metadata(ancestor)?;
        if m.file_type().is_symlink() {
            return Err(error("symlinked managed ancestry"));
        }
        if m.is_dir() && m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0 {
            return Err(error("writable managed ancestry"));
        }
    }
    let m = fs::symlink_metadata(path)?;
    if m.uid() != unsafe { nix::libc::getuid() }
        || m.mode() & 0o077 != 0
        || m.is_dir() != directory
        || (!directory && !m.is_file())
    {
        return Err(error("unowned or nonprivate managed state"));
    }
    Ok(())
}
fn private_dir(path: &Path) -> io::Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    secure(path, true)
}
fn durable(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if path.exists() {
        secure(path, false)?;
    }
    let temp = path.with_extension(format!("tmp-{}", random()?));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&temp)?;
    serde_json::to_writer(&mut f, value).map_err(error)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().ok_or_else(|| error("missing state parent"))?)?.sync_all()?;
    Ok(())
}
pub fn process(pid: u32) -> io::Result<Option<Identity>> {
    let mut c = Command::new("/bin/ps");
    c.args(["-p", &pid.to_string(), "-o", "lstart=,comm="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    match run_with_timeout(c, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) => {
            let s = String::from_utf8(o.stdout).map_err(error)?;
            let words: Vec<_> = s.split_whitespace().collect();
            if words.is_empty() && o.status.code() == Some(1) {
                let mut all = Command::new("/bin/ps");
                all.args(["-axo", "pid="]);
                return match run_with_timeout(all, Duration::from_secs(3)) {
                    CommandOutcome::Completed(o) if o.status.success() => {
                        let rows = String::from_utf8(o.stdout).map_err(error)?;
                        if rows.split_whitespace().any(|s| s == pid.to_string()) {
                            Err(error("process temporarily unreachable"))
                        } else {
                            Ok(None)
                        }
                    }
                    _ => Err(error("absence cannot be verified")),
                };
            }
            if !o.status.success() || words.len() < 6 {
                return Err(error("process identity incomplete"));
            }
            Ok(Some(Identity {
                pid,
                start: words[..5].join(" "),
                executable: words[5..].join(" "),
                boot: boot_identity()?,
            }))
        }
        _ => Err(error("process identity probe failed")),
    }
}
fn live(i: &Identity) -> io::Result<bool> {
    Ok(process(i.pid)?.is_some_and(|current| current.start == i.start && current.boot == i.boot))
}
/// HORO-1914: `Orphaned` may only be declared when liveness was
/// conclusively checked and found absent — never merely because a `ps`/
/// `sysctl` probe failed to answer (`probe_uncertain`), which is a real,
/// observed outcome under genuine host contention (concurrent process
/// spawns racing the probe's bounded timeout), not evidence the owner is
/// gone.
fn orphaned_after_probe(has_live: bool, probe_uncertain: bool, has_claims: bool) -> bool {
    !has_live && !probe_uncertain && has_claims
}
#[cfg(test)]
mod orphaned_after_probe_tests {
    use super::orphaned_after_probe;
    #[test]
    fn confirmed_absence_with_claims_is_orphaned() {
        assert!(orphaned_after_probe(false, false, true));
    }
    #[test]
    fn inconclusive_probe_is_never_orphaned_even_with_claims() {
        assert!(!orphaned_after_probe(false, true, true));
    }
    #[test]
    fn a_live_owner_is_never_orphaned_regardless_of_probe_uncertainty() {
        assert!(!orphaned_after_probe(true, true, true));
        assert!(!orphaned_after_probe(true, false, true));
    }
    #[test]
    fn no_claims_at_all_is_never_orphaned() {
        assert!(!orphaned_after_probe(false, false, false));
    }
}
impl Store {
    pub fn current() -> io::Result<Self> {
        Ok(Self {
            root: PathBuf::from(std::env::var_os("HOME").ok_or_else(|| error("HOME unavailable"))?)
                .canonicalize()?
                .join("Library/Application Support/Glomeris/resources"),
        })
    }
    pub fn init(&self) -> io::Result<()> {
        private_dir(&self.root)?;
        for n in ["records", "data", "barriers", "inputs"] {
            private_dir(&self.root.join(n))?;
        }
        Ok(())
    }
    /// Read-only bounded traversal; the guarded caller advances after each attempt.
    pub(crate) fn batch_ids(&self, phase: &str) -> io::Result<Vec<String>> {
        let ids = self.ids()?;
        let path = self.cursor_path(phase)?;
        let cursor = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
            Ok(_) => {
                secure(&path, false)?;
                let value: TraversalCursor = serde_json::from_reader(
                    OpenOptions::new()
                        .read(true)
                        .custom_flags(nix::libc::O_NOFOLLOW)
                        .open(path)?,
                )
                .map_err(error)?;
                if value.schema != 1 || !valid_id(&value.last_id) {
                    return Err(error("invalid traversal cursor"));
                }
                Some(value.last_id)
            }
        };
        let start = cursor.map_or(0, |last| ids.partition_point(|id| id <= &last));
        Ok(ids[start..]
            .iter()
            .chain(ids[..start].iter())
            .take(64)
            .cloned()
            .collect())
    }
    fn cursor_path(&self, phase: &str) -> io::Result<PathBuf> {
        if !matches!(phase, "sweep" | "reconcile") {
            return Err(error("invalid traversal phase"));
        }
        Ok(self.root.join(format!("{phase}-cursor.json")))
    }
    fn advance_cursor(
        &self,
        phase: &str,
        id: &str,
        _lock: &crate::executor::lock::ExecutionLockGuard,
    ) -> io::Result<()> {
        durable(
            &self.cursor_path(phase)?,
            &TraversalCursor {
                schema: 1,
                last_id: id.to_owned(),
            },
        )
    }
    fn record_path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(error("invalid resource ID"));
        }
        Ok(self.root.join("records").join(format!("{id}.json")))
    }
    pub fn read(&self, id: &str) -> io::Result<Record> {
        secure(&self.root, true)?;
        let p = self.record_path(id)?;
        secure(&p, false)?;
        let r: Record = serde_json::from_reader(
            OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(p)?,
        )
        .map_err(error)?;
        if r.schema != 1
            || r.id != id
            || !valid_id(&r.generation)
            || r.class != "managed_generated"
            || r.retirement_history.len() > 64
            || r.retirement_history
                .iter()
                .any(|a| !valid_id(&a.attempt_id) || !valid_id(&a.generation))
            || r.recipe.trim().is_empty()
            || !r.source_root.is_absolute()
            || r.source_head.len() != 40
            || !r.source_head.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error("invalid managed record"));
        }
        Ok(r)
    }
    pub fn save(&self, r: &Record) -> io::Result<()> {
        durable(&self.record_path(&r.id)?, r)
    }
    pub fn path(&self, r: &Record) -> PathBuf {
        self.root.join("data").join(&r.id).join(&r.generation)
    }
    pub fn policy(&self) -> io::Result<RetentionPolicy> {
        let p = self.root.join("policy.json");
        if !p.exists() {
            return Ok(RetentionPolicy::default());
        }
        secure(&p, false)?;
        let v: RetentionPolicy = serde_json::from_reader(
            OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(p)?,
        )
        .map_err(error)?;
        validate_policy(&v)?;
        Ok(v)
    }
    pub fn apply_policy(&self, v: &RetentionPolicy) -> io::Result<()> {
        validate_policy(v)?;
        self.init()?;
        let _l = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
        durable(&self.root.join("policy.json"), v)
    }
    pub fn create(&self, source: &Path, recipe: &str) -> io::Result<Record> {
        if recipe.trim().is_empty() || recipe.len() > 512 {
            return Err(error("reconstruction recipe required (max512 characters)"));
        }
        self.init()?;
        let _l = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
        let source_root = fs::canonicalize(source)?;
        let source_head = source_revision(&source_root, None)?;
        let id = random()?;
        let generation = random()?;
        let base = self.root.join("data").join(&id);
        private_dir(&base)?;
        let path = base.join(&generation);
        private_dir(&path)?;
        let m = fs::symlink_metadata(&path)?;
        let r = Record {
            schema: 1,
            id,
            generation,
            class: "managed_generated".into(),
            recipe: recipe.into(),
            source_root,
            source_head,
            dev: m.dev(),
            ino: m.ino(),
            last_meaningful_use: now(),
            claims: vec![],
            read_claims: vec![],
            sealed: None,
            producer: None,
            production_started: false,
            retirement_started: false,
            retirement_history: vec![],
            reclaimed: false,
            observed_free_delta: None,
        };
        self.save(&r)?;
        Ok(r)
    }
    pub fn barrier(&self, id: &str, exclusive: bool) -> io::Result<Flock<File>> {
        let p = self.root.join("barriers").join(format!("{id}.lock"));
        if !valid_id(id) {
            return Err(error("invalid ID"));
        }
        if p.exists() {
            secure(&p, false)?;
        }
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(p)?;
        Flock::lock(
            f,
            if exclusive {
                FlockArg::LockExclusiveNonblock
            } else {
                FlockArg::LockSharedNonblock
            },
        )
        .map_err(|(_, e)| io::Error::from(e))
    }
    pub fn identity(&self, r: &Record) -> io::Result<()> {
        secure(&self.path(r), true)?;
        let m = fs::symlink_metadata(self.path(r))?;
        if m.dev() != r.dev || m.ino() != r.ino {
            return Err(error("resource generation identity changed"));
        }
        Ok(())
    }
    pub fn inspect(&self, r: &Record, time: u64) -> Inspection {
        let mut blockers = vec![];
        let threshold = self
            .policy()
            .ok()
            .and_then(|p| p.classes.get(&r.class).map(|c| c.idle_threshold_secs))
            .unwrap_or(86400);
        let mut state = Lifecycle::Unknown;
        let mut has_live = false;
        let mut stalled = false;
        // HORO-1914: a probe failure (e.g. a transient `ps`/`sysctl` timeout
        // under real host contention) must never be read as "the process is
        // gone" — only a definitive `Ok(false)`/`Ok(None)` may say that. If
        // any identity's liveness could not be determined, `Orphaned` below
        // is withheld; the inconclusive claim stays `Unknown` (its already
        // appended blocker records why) rather than becoming eligible for
        // unattended reclaim on a false "nobody's using this" reading.
        let mut probe_uncertain = false;
        if r.reclaimed {
            return Inspection {
                id: r.id.clone(),
                generation: r.generation.clone(),
                path: self.path(r),
                lifecycle: Lifecycle::Reclaimed,
                eligible: false,
                blockers: vec![],
                last_meaningful_use: r.last_meaningful_use,
                inactivity_eligible: false,
            };
        }
        if let Err(e) = self.identity(r) {
            blockers.push(e.to_string());
        }
        if let Err(e) = source_revision(&r.source_root, Some(&r.source_head)) {
            blockers.push(e.to_string());
        }
        if let Err(e) = verify_producer(r) {
            blockers.push(e.to_string());
        }
        if r.claims.iter().any(|c| !c.released) {
            blockers.push(
                "native agent consumer coverage incomplete after an unconfirmed release".into(),
            );
        }
        if r.read_claims.iter().any(|c| !c.ended) {
            blockers.push("core artifact access has not ended; reconciliation required".into());
        }
        let identities: Vec<_> = r
            .claims
            .iter()
            .filter(|c| !c.released)
            .flat_map(|c| {
                std::iter::once(&c.holder)
                    .chain(std::iter::once(&c.owner))
                    .chain(c.children.iter())
            })
            .chain(r.read_claims.iter().filter(|c| !c.ended).map(|c| &c.holder))
            .collect();
        let snapshot = process_snapshot(&identities);
        for c in r.read_claims.iter().filter(|c| !c.ended) {
            match snapshot.as_ref().map(|map| {
                map.get(&c.holder.pid).is_some_and(|current| {
                    current.start == c.holder.start && current.boot == c.holder.boot
                })
            }) {
                Ok(true) => has_live = true,
                Ok(false) => {}
                Err(e) => {
                    probe_uncertain = true;
                    blockers.push(e.to_string());
                }
            }
        }
        for c in r.claims.iter().filter(|c| !c.released) {
            if c.pending_children > 0 {
                blockers.push("unconfirmed child launch; recovery information preserved".into());
            }
            for i in std::iter::once(&c.holder)
                .chain(std::iter::once(&c.owner))
                .chain(c.children.iter())
            {
                match snapshot.as_ref().map(|map| {
                    map.get(&i.pid)
                        .is_some_and(|current| current.start == i.start && current.boot == i.boot)
                }) {
                    Ok(true) => has_live = true,
                    Ok(false) => {}
                    Err(e) => {
                        probe_uncertain = true;
                        blockers.push(e.to_string());
                    }
                }
            }
            stalled |= c.health_faults.len() >= 2;
        }
        if has_live {
            state = if time.saturating_sub(r.last_meaningful_use) < threshold {
                Lifecycle::Active
            } else if stalled {
                Lifecycle::Stalled
            } else {
                Lifecycle::Idle
            };
            blockers
                .push("valid live owner or registered consumer (renewal is not progress)".into());
        } else if probe_uncertain {
            blockers.push(
                "owner/holder liveness could not be confirmed; withholding orphaned status".into(),
            );
        } else if orphaned_after_probe(has_live, probe_uncertain, !r.claims.is_empty()) {
            state = Lifecycle::Orphaned;
        }
        let policy = match self.policy() {
            Ok(p) => p,
            Err(e) => {
                blockers.push(e.to_string());
                RetentionPolicy::default()
            }
        };
        let threshold = policy
            .classes
            .get(&r.class)
            .map_or(86400, |c| c.idle_threshold_secs);
        if time < r.last_meaningful_use || time - r.last_meaningful_use < threshold {
            blockers.push("meaningful-use inactivity threshold not reached".into());
        }
        match (&r.sealed, artifacts(&self.path(r), r.dev)) {
            (Some(expected), Ok(actual)) if expected == &actual => {}
            (Some(_), Ok(_)) => {
                blockers.push("artifact generation changed after reconstruction attestation".into())
            }
            (None, _) => blockers.push("generated-only reconstruction manifest missing".into()),
            (_, Err(e)) => blockers.push(e.to_string()),
        }
        if r.retirement_started {
            blockers.push("interrupted retirement requires reconciliation".into());
        }
        let eligible = blockers.is_empty();
        if eligible {
            state = Lifecycle::Reclaimable;
        }
        Inspection {
            id: r.id.clone(),
            generation: r.generation.clone(),
            path: self.path(r),
            lifecycle: state,
            eligible,
            blockers,
            last_meaningful_use: r.last_meaningful_use,
            inactivity_eligible: time >= r.last_meaningful_use
                && time - r.last_meaningful_use >= threshold,
        }
    }
    pub fn ids(&self) -> io::Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        secure(&self.root, true)?;
        let mut ids = vec![];
        for e in fs::read_dir(self.root.join("records"))? {
            let p = e?.path();
            if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                if valid_id(s) {
                    ids.push(s.into());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
    pub fn reconcile(&self) -> io::Result<Vec<Inspection>> {
        let _l = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
        let mut rows = vec![];
        let started = std::time::Instant::now();
        for id in self.batch_ids("reconcile")? {
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
            self.advance_cursor("reconcile", &id, &_l)?;
            let mut r = match self.read(&id) {
                Ok(r) => r,
                Err(e) => {
                    rows.push(Inspection {
                        id: id.clone(),
                        generation: String::new(),
                        path: self.record_path(&id)?,
                        lifecycle: Lifecycle::Unknown,
                        eligible: false,
                        blockers: vec![format!("managed record unavailable: {e}")],
                        last_meaningful_use: 0,
                        inactivity_eligible: false,
                    });
                    continue;
                }
            };
            let _barrier = match self.barrier(&id, true) {
                Ok(lock) => lock,
                Err(_) => {
                    rows.push(self.inspect(&r, now()));
                    continue;
                }
            };
            r = self.read(&id)?;
            let mut access_changed = false;
            for claim in r.read_claims.iter_mut().filter(|c| !c.ended) {
                if matches!(live(&claim.holder), Ok(false)) {
                    // This core-only operation never launches children or transfers source
                    // descriptors. Death ends source access, not successful export.
                    claim.ended = true;
                    access_changed = true;
                }
            }
            if access_changed {
                // Exact final access time is unavailable after interruption.
                // This one-time observed end is a conservative inactivity bound,
                // not a renewable heartbeat or an invented export success.
                r.last_meaningful_use = r.last_meaningful_use.max(now());
                self.save(&r)?;
            }
            if r.retirement_started && verified_absent(&self.path(&r))? {
                r.reclaimed = true;
                r.retirement_started = false;
                if let Some(attempt) = r.retirement_history.last_mut() {
                    attempt.completed_at = Some(now());
                }
                self.save(&r)?;
            } else if r.retirement_started
                && r.claims.is_empty()
                && r.read_claims.iter().all(|c| c.ended)
                && r.retirement_history.last().is_some_and(|a| {
                    a.generation == r.generation
                        && a.dev == r.dev
                        && a.ino == r.ino
                        && a.completed_at.is_none()
                })
                && r.sealed.as_ref() == r.producer.as_ref().map(|p| &p.outputs)
                && self.identity(&r).is_ok()
                && verify_producer(&r).is_ok()
                && source_revision(&r.source_root, Some(&r.source_head)).is_ok()
            {
                // Only a complete unchanged generation can re-enter admission.
                // No deletion/permit is performed here; subsequent policy is fresh.
                r.retirement_started = false;
                if let Some(attempt) = r.retirement_history.last_mut() {
                    attempt.retry_queued_at = Some(now());
                }
                self.save(&r)?;
            }
            if !r.reclaimed && !r.retirement_started && r.sealed.is_none() {
                let view = self.inspect(&r, now());
                let owner_unknown = view.blockers.iter().any(|s| {
                    s.contains("consumer")
                        || s.contains("child")
                        || s.contains("identity")
                        || s.contains("probe")
                        || s.contains("source")
                        || s.contains("commit")
                });
                if !owner_unknown {
                    {
                        if self.identity(&r).is_ok()
                            && verify_producer(&r).is_ok()
                            && source_revision(&r.source_root, Some(&r.source_head)).is_ok()
                        {
                            if let Ok(manifest) = artifacts(&self.path(&r), r.dev) {
                                if let Some(last) =
                                    manifest.values().map(|a| a.mtime.max(0) as u64).max()
                                {
                                    r.last_meaningful_use = r.last_meaningful_use.max(last);
                                }
                                r.sealed = Some(manifest);
                                self.save(&r)?;
                            }
                        }
                    }
                }
            }
            rows.push(self.inspect(&r, now()));
        }
        if let Err(e) = reconcile_audit(self, &_l) {
            // Not fatal to ownership reconciliation: the open attempts stay open
            // and the next pass retries, which is the honest state meanwhile.
            eprintln!("managed audit reconciliation will retry: {e}");
        }
        Ok(rows)
    }
}
fn validate_policy(p: &RetentionPolicy) -> io::Result<()> {
    if p.schema != 1
        || p.max_actions == 0
        || p.max_actions > 25
        || p.max_bytes == 0
        || p.max_bytes > 64 * 1024 * 1024 * 1024
        || p.classes.keys().any(|s| s != "managed_generated")
    {
        return Err(error(
            "unsupported class or budget; no blanket authorization",
        ));
    }
    Ok(())
}
/// Only source-free, non-executable compiler object/archive outputs are in the initial capability.
/// Other cache contents require a separate proven producer contract, not filename-based admission.
fn artifacts(root: &Path, dev: u64) -> io::Result<BTreeMap<String, Artifact>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    let mut count = 0;
    while let Some(p) = pending.pop() {
        for e in fs::read_dir(p)? {
            let e = e?;
            let p = e.path();
            let m = fs::symlink_metadata(&p)?;
            count += 1;
            if count > 10000
                || m.dev() != dev
                || m.file_type().is_symlink()
                || m.uid() != unsafe { nix::libc::getuid() }
            {
                return Err(error("artifact boundary/identity inspection incomplete"));
            }
            if m.is_dir() {
                if e.file_name().to_str() == Some(".git") {
                    return Err(error("unique Git state protected"));
                }
                pending.push(p);
                continue;
            }
            if !m.is_file() || m.nlink() != 1 || m.mode() & 0o111 != 0 {
                return Err(error("unique, linked or executable output protected"));
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            let (header, n) = artifact_header(&p, &m)?;
            let object = ext == "o"
                && n >= 4
                && (*b"\x7fELF" == header[..4]
                    || [
                        [0xcf, 0xfa, 0xed, 0xfe],
                        [0xce, 0xfa, 0xed, 0xfe],
                        [0xfe, 0xed, 0xfa, 0xcf],
                        [0xfe, 0xed, 0xfa, 0xce],
                    ]
                    .iter()
                    .any(|v| v == &header[..4]));
            let archive = matches!(ext, "a" | "rlib") && n == 8 && &header == b"!<arch>\n";
            let metadata = ext == "rmeta" && n >= 4 && &header[..4] == b"rust";
            if !(object || archive || metadata) {
                return Err(error("contents outside verified compiler-output capability; preserve unique/unknown data"));
            }
            let relative = p
                .strip_prefix(root)
                .map_err(error)?
                .to_string_lossy()
                .into_owned();
            out.insert(
                relative,
                Artifact {
                    dev: m.dev(),
                    ino: m.ino(),
                    len: m.len(),
                    mtime: m.mtime(),
                    mtime_nsec: m.mtime_nsec(),
                },
            );
        }
    }
    Ok(out)
}
fn artifact_header(path: &Path, expected: &fs::Metadata) -> io::Result<([u8; 8], usize)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)?;
    let identity = Artifact {
        dev: expected.dev(),
        ino: expected.ino(),
        len: expected.len(),
        mtime: expected.mtime(),
        mtime_nsec: expected.mtime_nsec(),
    };
    access::validate_open_artifact(&file, &identity)?;
    let mut header = [0u8; 8];
    let count = file.read(&mut header)?;
    access::validate_open_artifact(&file, &identity)?;
    Ok((header, count))
}
pub fn eligible_path(path: &Path, time: SystemTime) -> io::Result<()> {
    let store = Store::current()?;
    let r = record_for_path(&store, path)?;
    let view = store.inspect(
        &r,
        time.duration_since(UNIX_EPOCH).map_err(error)?.as_secs(),
    );
    if !view.eligible {
        return Err(error(view.blockers.join("; ")));
    }
    Ok(())
}
fn record_for_path(store: &Store, path: &Path) -> io::Result<Record> {
    let base = store.root.join("data");
    let relative = path.strip_prefix(&base).map_err(error)?;
    let mut components = relative.components();
    let normal = |part: Option<std::path::Component<'_>>| match part {
        Some(std::path::Component::Normal(value)) => value
            .to_str()
            .filter(|value| valid_id(value))
            .map(str::to_owned),
        _ => None,
    };
    let id = normal(components.next()).ok_or_else(|| error("invalid enrolled resource path"))?;
    let generation =
        normal(components.next()).ok_or_else(|| error("invalid enrolled generation path"))?;
    if components.next().is_some() {
        return Err(error("resource path outside exact generation"));
    }
    let record = store.read(&id)?;
    if record.generation != generation || store.path(&record) != path {
        return Err(error("resource enrollment identity changed"));
    }
    Ok(record)
}

/// Single-use admission minted only by the policy-authorized managed sweep.
/// Private fields prevent a generic execution-lock holder granting managed authority.
pub(crate) struct ManagedRetirementPermit<'a> {
    resource: crate::evidence::ResourceId,
    generation: String,
    fingerprint: crate::evidence::ResourceFingerprint,
    action: crate::actions::ActionId,
    admitted_bytes: u64,
    guard: RetirementGuard,
    _authority: &'a crate::executor::lock::ExecutionLockGuard,
}
impl ManagedRetirementPermit<'_> {
    pub(crate) fn into_guard(
        self,
        action: &dyn crate::actions::Action,
        approval: &crate::policy::approval::Approval,
    ) -> io::Result<RetirementGuard> {
        let size = self
            .guard
            .record
            .producer
            .as_ref()
            .map(|p| p.outputs.values().map(|a| a.len).sum::<u64>());
        if approval.decision().resource != self.resource
            || approval.fingerprint() != &self.fingerprint
            || action.id() != self.action
            || self.guard.record.generation != self.generation
            || size != Some(self.admitted_bytes)
        {
            return Err(error("managed retirement admission identity mismatch"));
        }
        Ok(self.guard)
    }
}
pub struct RetirementGuard {
    _barrier: Flock<File>,
    store: Store,
    record: Record,
}
impl RetirementGuard {
    pub fn acquire(
        path: &Path,
        _authority: &crate::executor::lock::ExecutionLockGuard,
    ) -> io::Result<Self> {
        let store = Store::current()?;
        let record = record_for_path(&store, path)?;
        let barrier = store.barrier(&record.id, true)?;
        let record = store.read(&record.id)?;
        eligible_path(path, SystemTime::now())?;
        Ok(Self {
            _barrier: barrier,
            store,
            record,
        })
    }
    pub fn intent(&mut self) -> io::Result<()> {
        if self.record.retirement_history.len() >= 64 {
            return Err(error(
                "retirement attempt bound reached; preserve recovery history",
            ));
        }
        self.record.retirement_history.push(RetirementAttempt {
            attempt_id: random()?,
            generation: self.record.generation.clone(),
            dev: self.record.dev,
            ino: self.record.ino,
            intent_at: now(),
            retry_queued_at: None,
            completed_at: None,
        });
        self.record.retirement_started = true;
        self.store.save(&self.record)
    }
    pub fn finish(&mut self, delta: Option<i64>) -> io::Result<()> {
        if verified_absent(&self.store.path(&self.record))? {
            self.record.reclaimed = true;
            self.record.retirement_started = false;
            if let Some(attempt) = self.record.retirement_history.last_mut() {
                attempt.completed_at = Some(now());
            }
            self.record.observed_free_delta = delta;
            self.store.save(&self.record)?;
        }
        Ok(())
    }
}
pub fn protects_other_kind(path: &Path) -> bool {
    Store::current().is_ok_and(|s| {
        path.starts_with(s.root.join("data")) || s.root.join("data").starts_with(path)
    })
}
fn write_reply(value: &impl Serialize) -> io::Result<()> {
    let mut o = io::stdout().lock();
    serde_json::to_writer(&mut o, value).map_err(error)?;
    o.write_all(b"\n")?;
    o.flush()
}
pub fn hold(
    store: &Store,
    id: &str,
    generation: &str,
    host: &str,
    session: &str,
    owner: u32,
) -> io::Result<()> {
    if !matches!(host, "claude" | "codex" | "worker")
        || session.trim().is_empty()
        || session.len() > 128
    {
        return Err(error("native host/session identity required"));
    }
    let global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let mut r = store.read(id)?;
    store.identity(&r)?;
    if r.generation != generation || r.retirement_started || r.reclaimed {
        return Err(error("stale or retiring generation"));
    }
    let _barrier = store.barrier(id, false)?;
    let token = random()?;
    let claim = Claim {
        token: token.clone(),
        host: host.into(),
        session: session.into(),
        holder: process(std::process::id())?.ok_or_else(|| error("holder identity missing"))?,
        owner: process(owner)?.ok_or_else(|| error("owner identity missing"))?,
        lease_until: now() + 90,
        progress_sequence: 0,
        children: vec![],
        pending_children: 0,
        released: false,
        health_faults: vec![],
    };
    let _metadata = store.metadata_barrier(id)?;
    r = store.read(id)?;
    if r.claims.len() >= 64 {
        return Err(error("owner coverage bound reached"));
    }
    // Native exposure can create escaped descendants not covered by the lease.
    // A later release is not independent proof of complete consumer closure.
    if let Some(proof) = &mut r.producer {
        proof.closed_consumers = false;
    }
    r.claims.push(claim);
    r.last_meaningful_use = now();
    r.sealed = None;
    store.save(&r)?;
    drop(_metadata);
    drop(global);
    write_reply(
        &serde_json::json!({"status":"held","id":id,"generation":generation,"path":store.path(&r)}),
    )?;
    for line in io::stdin().lock().lines() {
        let request: serde_json::Value = serde_json::from_str(&line?).map_err(error)?;
        let op = request["op"]
            .as_str()
            .ok_or_else(|| error("operation missing"))?;
        if op == "hold" {
            continue;
        }
        let metadata = match store.metadata_barrier(id) {
            Ok(lock) => lock,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                write_reply(
                    &serde_json::json!({"status":"busy","reason":"resource metadata busy; lease barrier retained"}),
                )?;
                continue;
            }
            Err(e) => {
                write_reply(&serde_json::json!({"status":"refused","reason":e.to_string()}))?;
                continue;
            }
        };
        r = store.read(id)?;
        if r.generation != generation {
            return Err(error("generation changed"));
        }
        let index = r
            .claims
            .iter()
            .position(|c| c.token == token && !c.released)
            .ok_or_else(|| error("claim released or replaced"))?;
        let result: io::Result<()> = (|| {
            match op {
                "renew" => {
                    r.claims[index].lease_until = now() + 90;
                }
                "progress" => {
                    let sequence = request["sequence"]
                        .as_u64()
                        .ok_or_else(|| error("progress sequence required"))?;
                    if sequence <= r.claims[index].progress_sequence {
                        return Err(error("no meaningful progress advancement"));
                    }
                    r.claims[index].progress_sequence = sequence;
                    r.last_meaningful_use = now();
                }
                "health" => {
                    let mut faults: Vec<String> =
                        serde_json::from_value(request["faults"].clone()).map_err(error)?;
                    faults.sort();
                    faults.dedup();
                    if faults.len() < 2
                        || faults.iter().any(|s| {
                            !matches!(
                                s.as_str(),
                                "checkpoint_stale" | "health_probe_failed" | "dependency_failed"
                            )
                        })
                    {
                        return Err(error("independent stronger health evidence required"));
                    }
                    r.claims[index].health_faults = faults;
                }
                "child_pending" => {
                    if r.claims[index].children.len() + r.claims[index].pending_children as usize
                        >= 32
                    {
                        return Err(error("child coverage bound reached"));
                    }
                    r.claims[index].pending_children = r.claims[index]
                        .pending_children
                        .checked_add(1)
                        .ok_or_else(|| error("child count overflow"))?;
                }
                "register_child" => {
                    let pid = request["pid"]
                        .as_u64()
                        .and_then(|p| u32::try_from(p).ok())
                        .ok_or_else(|| error("child PID required"))?;
                    if r.claims[index].pending_children == 0 {
                        return Err(error("child launch intent missing"));
                    }
                    let child = process(pid)?.ok_or_else(|| error("child identity unavailable"))?;
                    r.claims[index].children.push(child);
                    r.claims[index].pending_children -= 1;
                }
                "release" => {
                    if r.claims[index].pending_children > 0 {
                        return Err(error("unconfirmed children preserved"));
                    }
                    for child in &r.claims[index].children {
                        if live(child)? || group_live(child.pid)? {
                            return Err(error("registered child or process group remains active"));
                        }
                    }
                    r.claims[index].released = true;
                    if r.claims.iter().all(|c| c.released) {
                        store.identity(&r)?;
                        source_revision(&r.source_root, Some(&r.source_head))?;
                        if verify_producer(&r).is_ok() {
                            r.sealed = Some(artifacts(&store.path(&r), r.dev)?);
                        } else {
                            r.sealed = None;
                        }
                    }
                    r.last_meaningful_use = now();
                }
                _ => return Err(error("unknown holder operation")),
            }
            store.save(&r)
        })();
        drop(metadata);
        match result {
            Ok(()) => {
                write_reply(&serde_json::json!({"status":"ok","op":op}))?;
                if op == "release" {
                    return Ok(());
                }
            }
            Err(e) => write_reply(&serde_json::json!({"status":"refused","reason":e.to_string()}))?,
        }
    }
    Ok(())
}
pub fn seal(store: &Store, id: &str, generation: &str) -> io::Result<()> {
    let _global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let mut r = store.read(id)?;
    if r.generation != generation || r.retirement_started || r.reclaimed {
        return Err(error("stale generation"));
    }
    let _barrier = store.barrier(id, true)?;
    for c in r.claims.iter().filter(|c| !c.released) {
        if c.pending_children > 0 {
            return Err(error("child coverage incomplete"));
        }
        for p in std::iter::once(&c.holder)
            .chain(std::iter::once(&c.owner))
            .chain(c.children.iter())
        {
            if live(p)? {
                return Err(error("owner/consumer not released"));
            }
        }
    }
    store.identity(&r)?;
    verify_producer(&r)?;
    r.sealed = Some(artifacts(&store.path(&r), r.dev)?);
    r.last_meaningful_use = now();
    store.save(&r)
}
#[derive(Debug, Serialize)]
pub struct SweepItem {
    pub id: String,
    pub lifecycle: Lifecycle,
    pub status: String,
    pub blockers: Vec<String>,
    pub logical_bytes_removed: u64,
    pub physical_free_delta: Option<i64>,
}
pub fn sweep(store: &Store) -> io::Result<Vec<SweepItem>> {
    use crate::actions::ManagedGeneratedClean;
    use crate::evidence::correlate::DefaultEvidenceCollector;
    use crate::evidence::model::{ResourceId, ResourceKind, ResourceLocator};
    use crate::policy::{classify, PolicyClass, PolicyConfig};
    let _lock = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let policy = store.policy()?;
    let envelope = crate::autopilot::load_envelope().map_err(error)?;
    if !envelope.is_enabled()
        || !envelope.starts_unprompted()
        || !envelope.permits_kind(crate::evidence::ResourceKind::ManagedGenerated)
    {
        return Ok(vec![]);
    }
    if let Err(reason) = crate::autopilot::admits_pressure(&envelope, observed_pressure()) {
        return Err(error(reason));
    }
    let started = std::time::Instant::now();
    let mut ledger = crate::autopilot::BudgetLedger::for_envelope(&envelope);
    if !policy
        .classes
        .get("managed_generated")
        .is_some_and(|p| p.unattended)
    {
        return Ok(vec![]);
    }
    if store.ids()?.is_empty() {
        return Ok(vec![]);
    }
    let collector = DefaultEvidenceCollector::live(&[]);
    let cfg = PolicyConfig::default();
    let action = ManagedGeneratedClean;
    let mut rows = vec![];
    let mut actions = 0;
    let mut bytes = 0;
    for id in store.batch_ids("sweep")? {
        if started.elapsed() >= envelope.max_duration() {
            break;
        }
        let current = crate::autopilot::load_envelope().map_err(error)?;
        if crate::autopilot::admits_pressure(&current, observed_pressure()).is_err() {
            break;
        }
        if !current.is_enabled() || !current.starts_unprompted() {
            break;
        }
        store.advance_cursor("sweep", &id, &_lock)?;
        let r = match store.read(&id) {
            Ok(r) => r,
            Err(e) => {
                rows.push(SweepItem {
                    id: id.clone(),
                    lifecycle: Lifecycle::Unknown,
                    status: "preserved".into(),
                    blockers: vec![e.to_string()],
                    logical_bytes_removed: 0,
                    physical_free_delta: None,
                });
                continue;
            }
        };
        let view = store.inspect(&r, now());
        let mut item = SweepItem {
            id: id.clone(),
            lifecycle: view.lifecycle,
            status: "preserved".into(),
            blockers: view.blockers,
            logical_bytes_removed: 0,
            physical_free_delta: None,
        };
        if view.eligible && actions < policy.max_actions {
            let resource = ResourceId::new(
                ResourceKind::ManagedGenerated,
                ResourceLocator::Path(store.path(&r)),
            );
            let retirement = match RetirementGuard::acquire(&store.path(&r), &_lock) {
                Ok(g) => g,
                Err(e) => {
                    item.blockers.push(e.to_string());
                    rows.push(item);
                    continue;
                }
            };
            let t = SystemTime::now();
            let ev = crate::executor::build_fresh_evidence(&resource, &action, &collector, t);
            let size = ev.logical_bytes.observed().copied();
            if let Some(size) = size.filter(|s| bytes + s <= policy.max_bytes) {
                let decision = classify(&ev, &cfg, t);
                if decision.class == PolicyClass::AutoSafe
                    && matches!(
                        crate::autopilot::admit(
                            &current,
                            &ledger,
                            &decision,
                            Some(size),
                            started.elapsed()
                        ),
                        crate::autopilot::Admission::Admitted { .. }
                    )
                {
                    let approval =
                        crate::policy::approval::authorize(decision, ev.fingerprint.clone(), None)
                            .ok_or_else(|| error("policy authorization refused"))?;
                    // HORO-1920: nobody is watching this deletion, so a deletion
                    // that cannot be recorded must not happen. The durable
                    // `attempted` record is both the intent and the proof that the
                    // log accepts writes right now; nothing is charged to the
                    // budget and nothing is retired if it cannot be written.
                    if let Err(e) = audit_attempt(store, &ev.resource.to_string()) {
                        item.blockers
                            .push(format!("audit log not writable ({e}); nothing was deleted"));
                        rows.push(item);
                        continue;
                    }
                    ledger.charge(size, size);
                    actions += 1;
                    bytes += size;
                    let permit = ManagedRetirementPermit {
                        resource: ev.resource.clone(),
                        generation: r.generation.clone(),
                        fingerprint: ev.fingerprint.clone(),
                        action: crate::actions::ActionId("managed.clean.generated"),
                        admitted_bytes: size,
                        guard: retirement,
                        _authority: &_lock,
                    };
                    #[cfg(target_os = "macos")]
                    let report = crate::executor::execute_managed_owned(
                        &action,
                        &approval,
                        &collector,
                        &cfg,
                        SystemTime::now(),
                        Some(&crate::executor::MacosVolumeDelta),
                        permit,
                    );
                    #[cfg(not(target_os = "macos"))]
                    let report = crate::executor::execute_managed_owned(
                        &action,
                        &approval,
                        &collector,
                        &cfg,
                        SystemTime::now(),
                        None,
                        permit,
                    );
                    item.physical_free_delta = report.volume_free_delta.observed().copied();
                    note_outcome_audit(&mut item, record_sweep_audit(store, &report));
                    if let Some(warning) = &report.record_warning {
                        eprintln!("managed resource {} was deleted: {warning}", item.id);
                        item.blockers
                            .push(format!("{warning}; reconciliation will settle the record"));
                    }
                    if matches!(report.outcome, crate::executor::ExecutionOutcome::Succeeded) {
                        item.status = "reclaimed".into();
                        item.lifecycle = Lifecycle::Reclaimed;
                        item.logical_bytes_removed = size;
                    } else {
                        item.blockers
                            .push(format!("native executor: {:?}", report.outcome));
                    }
                } else {
                    item.blockers
                        .push(format!("native safety policy: {:?}", decision.reasons));
                }
            } else {
                item.blockers
                    .push("complete size or per-category budget unavailable".into());
            }
        }
        rows.push(item);
    }
    Ok(rows)
}
pub fn cli(args: &[String]) -> io::Result<()> {
    let store = Store::current()?;
    match args.first().map(String::as_str) {
        Some("native") => native::cli(&store, &args[1..]),
        Some("--help") | Some("help") => {
            println!("glomeris resource: create --source-root GIT_WORKSPACE --recipe RECIPE; hold ID GENERATION --host HOST --session SESSION --owner-pid PID; seal ID GENERATION; inspect; reconcile; sweep --unattended; policy show|apply FILE. All persisted ownership/policy is private. Only source-free generated compiler objects in core-created namespaces can be sealed; existing/unknown user paths are never enrolled.");
            Ok(())
        }
        Some("create") => {
            if args.len() != 5 || args[1] != "--source-root" || args[3] != "--recipe" {
                return Err(error("create --source-root GIT_WORKSPACE --recipe RECIPE"));
            }
            let r = store.create(Path::new(&args[2]), &args[4])?;
            write_reply(
                &serde_json::json!({"id":r.id,"generation":r.generation,"path":store.path(&r),"class":r.class}),
            )
        }
        Some("hold") => {
            if args.len() != 9
                || args[3] != "--host"
                || args[5] != "--session"
                || args[7] != "--owner-pid"
            {
                return Err(error(
                    "hold ID GENERATION --host HOST --session SESSION --owner-pid PID",
                ));
            }
            hold(
                &store,
                &args[1],
                &args[2],
                &args[4],
                &args[6],
                args[8].parse().map_err(error)?,
            )
        }
        Some("produce") => {
            if args.len() != 5 || args[3] != "--input" {
                return Err(error("produce ID GENERATION --input TRACKED_RS_FILE"));
            }
            produce(&store, &args[1], &args[2], Path::new(&args[4]))?;
            write_reply(&serde_json::json!({"status":"produced"}))
        }
        Some("export") => {
            if args.len() != 7 || args[3] != "--artifact" || args[5] != "--destination" {
                return Err(error("export ID GENERATION --artifact NAME --destination ABSOLUTE_PATH"));
            }
            let bytes = access::export(&store, &args[1], &args[2], &args[4], Path::new(&args[6]))?;
            write_reply(&serde_json::json!({"status":"exported","id":args[1],"generation":args[2],"bytes":bytes}))
        }
        Some("seal") => {
            if args.len() != 3 {
                return Err(error("seal ID GENERATION"));
            }
            seal(&store, &args[1], &args[2])?;
            write_reply(&serde_json::json!({"status":"sealed"}))
        }
        Some("inspect") => {
            let mut v = vec![];
            for id in store.ids()? {
                v.push(serde_json::to_value(store.inspect(&store.read(&id)?, now())).map_err(error)?);
            }
            v.extend(store.native_inspect()?);
            write_reply(&v)
        }
        Some("reconcile") => write_reply(&store.reconcile()?),
        Some("sweep")
            if args.get(1).map(String::as_str) == Some("--unattended") && args.len() == 2 =>
        {
            write_reply(&sweep(&store)?)
        }
        Some("policy")=>match args.get(1).map(String::as_str){
            Some("show") if args.len()==2=>write_reply(&store.policy()?),
            Some("apply")|Some("restore") if args.len()==3||args.len()==5=>{
                if args[1]=="restore" && args.len()!=5{return Err(error("restore requires expected-current snapshot"));}
                if args.len()==5&&args[3]!="--expected-current"{return Err(error("expected-current snapshot required"));}
                let raw=fs::read(&args[2])?;
                let bytes=if args[1]=="restore" && raw.is_empty(){serde_json::to_vec(&RetentionPolicy::default()).map_err(error)?}else{raw};
                let policy:RetentionPolicy=serde_json::from_slice(&bytes).map_err(error)?;validate_policy(&policy)?;store.init()?;
                let _lock=crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
                let path=store.root.join("policy.json");if args.len()==5{compare_snapshot(&path,Path::new(&args[4]))?;}
                durable_raw(&path,&bytes)?;write_reply(&store.policy()?)
            }, _=>Err(error("policy show|apply FILE [--expected-current SNAPSHOT]|restore FILE --expected-current SNAPSHOT"))
        },
        Some("authority")=>authority(&args[1..]),
        _ => Err(error("unknown managed resource operation")),
    }
}
impl Store {
    /// Where `actions.jsonl` lives: beside the resource store, which is the same
    /// file every other deleting path appends to. Derived from the store's own
    /// root rather than from `HOME`, so a store under test or in a fixture can
    /// never write to the real user's log.
    fn audit_path(&self) -> io::Result<PathBuf> {
        self.root
            .parent()
            .map(|dir| dir.join("actions.jsonl"))
            .ok_or_else(|| error("audit log location unavailable"))
    }
}

fn sweep_audit_record(
    resource_id: String,
    outcome: &str,
    abort_reason: Option<String>,
) -> crate::monitor::AuditRecord {
    crate::monitor::AuditRecord {
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        action_id: "managed.clean.generated".to_string(),
        resource_id,
        policy_label: crate::reporting::policy_label::PolicyLabel::AutoSafe
            .as_str()
            .to_string(),
        outcome: outcome.to_string(),
        abort_reason,
        actual_reclaimed_bytes: None,
        source: crate::monitor::ActionSource::ManagedSweep.to_string(),
        model_rank: None,
        host_integrity_status: None,
        volume_free_delta: None,
    }
}

/// HORO-1920: the durable intent written before a deletion starts. An `Err`
/// means the audit log cannot be relied on, and the caller must not delete.
fn audit_attempt(store: &Store, resource_id: &str) -> io::Result<()> {
    crate::monitor::append_audit_record_durable(
        &store.audit_path()?,
        &sweep_audit_record(resource_id.to_string(), "attempted", None),
    )
}

/// The outcome record for a finished sweep deletion attempt, or `None` for an
/// outcome that is not a real execution.
fn sweep_outcome_record(
    report: &crate::executor::ExecutionReport,
) -> Option<crate::monitor::AuditRecord> {
    use crate::executor::ExecutionOutcome;
    let (outcome, abort_reason) = match &report.outcome {
        ExecutionOutcome::Succeeded => ("succeeded", None),
        ExecutionOutcome::Failed(_) => ("failed", None),
        ExecutionOutcome::AbortedByRevalidation(reason) => {
            ("aborted_by_revalidation", Some(format!("{reason:?}")))
        }
        ExecutionOutcome::DryRun => return None,
    };
    let mut record = sweep_audit_record(report.resource.to_string(), outcome, abort_reason);
    record.actual_reclaimed_bytes = match report.outcome {
        ExecutionOutcome::Succeeded => report.actual_reclaimed_bytes.observed().copied(),
        _ => None,
    };
    record.host_integrity_status = report
        .host_integrity
        .as_ref()
        .map(|h| h.status.as_str().to_string());
    record.volume_free_delta = report.volume_free_delta.observed().copied();
    Some(record)
}

/// Appends the outcome after the `attempted` intent. Strict: the result is
/// reported, because the caller must not present an unrecorded deletion as a
/// clean success.
fn record_sweep_audit(store: &Store, report: &crate::executor::ExecutionReport) -> io::Result<()> {
    match sweep_outcome_record(report) {
        Some(record) => crate::monitor::append_audit_record_durable(&store.audit_path()?, &record),
        None => Ok(()),
    }
}

/// Makes an outcome-append failure visible instead of silent. The deletion
/// already happened and is not undone; the log now holds `attempted` with no
/// outcome, which is the honest record, and the next reconcile closes it.
fn note_outcome_audit(item: &mut SweepItem, result: io::Result<()>) {
    if let Err(e) = result {
        eprintln!(
            "managed resource {} deleted but its audit outcome was not recorded: {e}",
            item.id
        );
        item.blockers.push(format!(
            "audit outcome not recorded ({e}); reconciliation will complete it"
        ));
    }
}

/// What to record for an open `attempted` record, or `None` to leave it open
/// (HORO-1920). Decided from what is on disk now, never from a flag that this
/// same reconcile pass may not have reached yet (the ownership pass is capped at
/// 64 records and 10 seconds, so `reclaimed` can lag a completed deletion):
///
/// * no ownership record for the resource, or presence cannot be established
///   ⇒ unprovable, so the attempt stays open rather than being guessed;
/// * the generation is gone ⇒ `succeeded`;
/// * the generation is still there ⇒ `interrupted`. This is inferred from
///   presence: it covers a deletion that never started and one cut off part-way.
fn audit_close_outcome(found: bool, absent: io::Result<bool>) -> Option<&'static str> {
    if !found {
        return None;
    }
    match absent {
        Ok(true) => Some("succeeded"),
        Ok(false) => Some("interrupted"),
        Err(_) => None,
    }
}

/// HORO-1920: closes `attempted` records that no outcome ever followed (the
/// process died, or the outcome append failed). Called with the execution lock
/// held, so no sweep is in flight and any open attempt is stale. Each pass
/// closes the attempts it can prove from [`audit_close_outcome`]; the rest stay
/// open and are retried on the next pass. Idempotent: once closed, an attempt is
/// no longer unmatched, so a second pass appends nothing. The window is the last
/// [`crate::monitor::UNMATCHED_ATTEMPT_WINDOW`] lines of the live and rotated
/// log; an attempt older than that is not chased.
fn reconcile_audit(
    store: &Store,
    _authority: &crate::executor::lock::ExecutionLockGuard,
) -> io::Result<()> {
    use crate::evidence::model::{ResourceId, ResourceKind, ResourceLocator};
    let path = store.audit_path()?;
    let open = crate::monitor::unmatched_attempts(
        &path,
        &crate::monitor::ActionSource::ManagedSweep.to_string(),
    );
    if open.is_empty() {
        return Ok(());
    }
    let mut records = vec![];
    for id in store.ids()? {
        if let Ok(r) = store.read(&id) {
            let key = ResourceId::new(
                ResourceKind::ManagedGenerated,
                ResourceLocator::Path(store.path(&r)),
            )
            .to_string();
            records.push((key, store.path(&r)));
        }
    }
    for attempt in open {
        let found = records.iter().find(|(key, _)| *key == attempt.resource_id);
        let absent = match found {
            Some((_, resource_path)) => verified_absent(resource_path),
            None => Ok(false),
        };
        let Some(outcome) = audit_close_outcome(found.is_some(), absent) else {
            continue;
        };
        let record = sweep_audit_record(attempt.resource_id, outcome, None);
        crate::monitor::append_audit_record_durable(&path, &record)?;
    }
    Ok(())
}
/// The monitor-log lines for one sweep's rows: reclamations, and refusals the
/// operator would otherwise never see. A persistently unwritable audit log makes
/// every sweep refuse to delete; without a line saying so that looks identical
/// to "nothing was eligible".
fn sweep_log_lines(rows: &[SweepItem]) -> Vec<String> {
    let mut lines = vec![];
    for row in rows {
        if row.status == "reclaimed" {
            lines.push(format!(
                "managed resource {} reclaimed; observed physical delta {:?}",
                row.id, row.physical_free_delta
            ));
        }
        if row.status != "reclaimed"
            && row
                .blockers
                .iter()
                .any(|b| b.contains("audit log not writable"))
        {
            lines.push(format!(
                "managed resource {} NOT cleaned: the audit log is not writable, and an unauditable deletion is refused",
                row.id
            ));
        }
    }
    lines
}
/// Called only from the existing monitor; absent/disabled policy does not initialize new state.
pub fn monitor_sweep() {
    match Store::current().and_then(|s| {
        if !s.root.exists() {
            return Ok(vec![]);
        }
        s.reconcile()?;
        sweep(&s)
    }) {
        Ok(rows) => {
            for line in sweep_log_lines(&rows) {
                eprintln!("{line}");
            }
        }
        Err(e) => eprintln!("managed resource reconciliation preserved data: {e}"),
    }
}
fn source_revision(root: &Path, pinned: Option<&str>) -> io::Result<String> {
    if !root.is_absolute()
        || fs::symlink_metadata(root)?.file_type().is_symlink()
        || !root.join("Cargo.toml").is_file()
    {
        return Err(error("reconstruction source workspace unavailable"));
    }
    let args = if let Some(head) = pinned {
        vec!["cat-file", "-e", head]
    } else {
        vec!["rev-parse", "HEAD"]
    };
    let mut c = Command::new("git");
    c.arg("--no-optional-locks").arg("-C").arg(root).args(args);
    match run_with_timeout(c, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() => {
            if let Some(head) = pinned {
                return Ok(head.into());
            }
            let head = String::from_utf8(o.stdout)
                .map_err(error)?
                .trim()
                .to_owned();
            let mut c = Command::new("git");
            c.arg("--no-optional-locks").arg("-C").arg(root).args([
                "status",
                "--porcelain",
                "--untracked-files=all",
            ]);
            match run_with_timeout(c, Duration::from_secs(3)) {
                CommandOutcome::Completed(o) if o.status.success() && o.stdout.is_empty() => {
                    Ok(head)
                }
                _ => Err(error(
                    "reconstruction source must be a clean committed workspace",
                )),
            }
        }
        _ => Err(error("pinned reconstruction commit unavailable")),
    }
}
fn group_live(pgid: u32) -> io::Result<bool> {
    let mut c = Command::new("/bin/ps");
    c.args(["-axo", "pid=,pgid="]);
    match run_with_timeout(c, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() => {
            let text = String::from_utf8(o.stdout).map_err(error)?;
            for line in text.lines() {
                let w: Vec<_> = line.split_whitespace().collect();
                if w.len() != 2 {
                    return Err(error("consumer group observation incomplete"));
                }
                if w[1].parse::<u32>().map_err(error)? == pgid {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Err(error("consumer group probe failed")),
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipEvidence {
    pub reclaimable: bool,
    pub blockers: Vec<String>,
}
pub fn collect_ownership(
    path: &Path,
    time: SystemTime,
) -> crate::evidence::probe::ProbeOutcome<OwnershipEvidence> {
    use crate::evidence::probe::{ProbeOutcome, ProbeReason};
    let result = (|| {
        let s = Store::current()?;
        let r = record_for_path(&s, path)?;
        let view = s.inspect(
            &r,
            time.duration_since(UNIX_EPOCH).map_err(error)?.as_secs(),
        );
        Ok::<_, io::Error>(OwnershipEvidence {
            reclaimable: view.eligible,
            blockers: view.blockers,
        })
    })();
    match result {
        Ok(v) => ProbeOutcome::Observed(v),
        Err(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
    }
}
fn verified_absent(path: &Path) -> io::Result<bool> {
    secure(
        path.parent()
            .ok_or_else(|| error("resource parent missing"))?,
        true,
    )?;
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
        Ok(_) => Ok(false),
    }
}
/// HORO-1916: per-attempt timeout for the `ps`/`sysctl` liveness probes
/// behind [`Store::inspect`] — unchanged from the original 3s so an
/// uncontended host sees no latency change.
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// HORO-1916: total wall-clock budget across the first attempt plus one
/// retry. 5s (a 2s allowance beyond the 3s first attempt) was sized from
/// measured evidence: under real contention, timeouts land right at the
/// 3.0-3.05s boundary, not far past it, so a short second window is
/// enough to let a transient scheduling spike clear. This stays well
/// inside the 20s CLI correlation-pass budget (HORO-1912) even for a
/// candidate whose probe needs the full retry.
const LIVENESS_PROBE_RETRY_BUDGET: Duration = Duration::from_secs(5);
fn boot_identity() -> io::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let outcome = run_with_timeout_retrying(
            || {
                let mut c = Command::new("/usr/sbin/sysctl");
                c.args(["-n", "kern.boottime"]);
                c
            },
            LIVENESS_PROBE_TIMEOUT,
            LIVENESS_PROBE_RETRY_BUDGET,
        );
        match outcome {
            CommandOutcome::Completed(o) if o.status.success() => Ok(String::from_utf8(o.stdout)
                .map_err(error)?
                .split('}')
                .next()
                .ok_or_else(|| error("boot identity malformed"))?
                .into()),
            _ => Err(error("boot identity unavailable")),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into())
    }
}
fn artifact_identity(path: &Path) -> io::Result<Artifact> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(error("retained input identity changed"));
    }
    Ok(Artifact {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        mtime: m.mtime(),
        mtime_nsec: m.mtime_nsec(),
    })
}
fn verify_producer(r: &Record) -> io::Result<()> {
    let proof = r
        .producer
        .as_ref()
        .ok_or_else(|| error("verified closed compiler producer provenance missing"))?;
    let store = Store::current()?;
    let expected_input = store
        .root
        .join("inputs")
        .join(format!("{}-{}", r.id, r.generation))
        .join("input.rs");
    if proof.input != expected_input {
        return Err(error(
            "producer input outside enrolled reconstruction generation",
        ));
    }
    secure(&proof.input, false)?;
    if !proof.closed_consumers || artifact_identity(&proof.input)? != proof.input_metadata {
        return Err(error("producer input/consumer coverage incomplete"));
    }
    if artifacts(&store.path(r), r.dev)? != proof.outputs {
        return Err(error(
            "outputs not bound to verified producer or unique state introduced",
        ));
    }
    Ok(())
}
/// Initial producer capability: one retained tracked Rust input, object-only rustc, no external/env dependencies.
/// General Cargo/agent outputs cannot be admitted by merely relabeling files as generated.
pub fn produce(store: &Store, id: &str, generation: &str, input: &Path) -> io::Result<()> {
    let _global = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let mut r = store.read(id)?;
    store.identity(&r)?;
    if r.generation != generation
        || r.retirement_started
        || r.reclaimed
        || r.production_started
        || r.producer.is_some()
        || !r.claims.is_empty()
    {
        return Err(error("stale or previously produced generation"));
    }
    if input.extension().and_then(|s| s.to_str()) != Some("rs")
        || input
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(error("tracked relative Rust input required"));
    }
    let _barrier = store.barrier(id, true)?;
    if fs::read_dir(store.path(&r))?.next().is_some() {
        return Err(error("unproven existing outputs cannot be adopted"));
    }
    source_revision(&r.source_root, Some(&r.source_head))?;
    r.production_started = true;
    store.save(&r)?;
    let mut git = Command::new("git");
    git.arg("--no-optional-locks")
        .arg("-C")
        .arg(&r.source_root)
        .args([
            "show",
            &format!("{}:{}", r.source_head, input.to_string_lossy()),
        ]);
    let bytes = match run_with_timeout(git, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() && o.stdout.len() <= 65536 => o.stdout,
        _ => return Err(error("bounded pinned input unavailable")),
    };
    let source_text = std::str::from_utf8(&bytes).map_err(error)?;
    // This first capability excludes macros/assembly that can hide file reads
    // outside rustc dep-info. Broader Rust producers need a stronger sandbox.
    for forbidden in [
        "asm",
        "include",
        "env!",
        "option_env",
        "macro_rules",
        "concat",
        "#![feature",
        "extern",
    ] {
        if source_text.contains(forbidden) {
            return Err(error(
                "source outside closed standalone producer capability",
            ));
        }
    }
    let inputs = store
        .root
        .join("inputs")
        .join(format!("{}-{}", id, generation));
    private_dir(&inputs)?;
    let source = inputs.join("input.rs");
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&source)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    let output = store.path(&r).join("artifact.o");
    let dep = inputs.join("dependencies.d");
    let mut version = Command::new("rustc");
    version
        .arg("--version")
        .current_dir(&inputs)
        .env("RUSTUP_AUTO_INSTALL", "0");
    let version = match run_with_timeout(version, Duration::from_secs(3)) {
        CommandOutcome::Completed(o) if o.status.success() => {
            String::from_utf8(o.stdout).map_err(error)?
        }
        _ => return Err(error("installed compiler unavailable")),
    };
    if !version.starts_with("rustc ") {
        return Err(error("unsupported compiler identity"));
    }
    let mut compiler = Command::new("rustc");
    compiler
        .args(["--crate-name", "managed_generated", "--crate-type", "lib"])
        .arg(format!(
            "--emit=obj={},dep-info={}",
            output.display(),
            dep.display()
        ))
        .arg(&source)
        .current_dir(&inputs)
        .env("RUSTUP_AUTO_INSTALL", "0");
    // Explicit capability budget for this NEW isolated producer, never an existing build.
    unsafe {
        compiler.pre_exec(|| {
            let limit = nix::libc::rlimit {
                rlim_cur: 32 * 1024 * 1024,
                rlim_max: 32 * 1024 * 1024,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match run_with_timeout(compiler, Duration::from_secs(60)) {
        CommandOutcome::Completed(o) if o.status.success() => {}
        _ => {
            return Err(error(
                "isolated compiler producer did not complete; preserve recovery inputs",
            ))
        }
    }
    let deps = fs::read_to_string(&dep)?.replace("\\ ", " ");
    let allowed = [
        format!("{}: {}", dep.display(), source.display()),
        format!("{}: {}", output.display(), source.display()),
        format!("{}:", source.display()),
    ];
    if deps
        .lines()
        .filter(|l| !l.is_empty())
        .any(|l| !allowed.iter().any(|expected| expected == l))
        || !deps.lines().any(|l| l == allowed[1])
    {
        return Err(error("external or environment compiler dependencies are not within closed reconstruction capability"));
    }
    let manifest = artifacts(&store.path(&r), r.dev)?;
    r.producer = Some(ProducerProof {
        input: source.clone(),
        input_metadata: artifact_identity(&source)?,
        compiler_version: version.trim().into(),
        outputs: manifest.clone(),
        closed_consumers: true,
    });
    r.sealed = Some(manifest);
    r.last_meaningful_use = now();
    store.save(&r)
}
impl Store {
    fn metadata_barrier(&self, id: &str) -> io::Result<Flock<File>> {
        if !valid_id(id) {
            return Err(error("invalid metadata resource ID"));
        }
        let p = self
            .root
            .join("barriers")
            .join(format!("{id}.metadata.lock"));
        if p.exists() {
            secure(&p, false)?;
        }
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(p)?;
        Flock::lock(f, FlockArg::LockExclusiveNonblock).map_err(|(_, e)| io::Error::from(e))
    }
}
fn lock_error(e: crate::executor::lock::LockError) -> io::Error {
    match e {
        crate::executor::lock::LockError::AlreadyHeld => {
            io::Error::new(io::ErrorKind::WouldBlock, "shared execution lock busy")
        }
        crate::executor::lock::LockError::Io(e) => e,
    }
}
fn observed_pressure() -> Option<crate::monitor::PressureState> {
    #[cfg(target_os = "macos")]
    {
        use crate::monitor::FsStat;
        crate::platform::macos::MacosFsStat
            .stat(Path::new("/"))
            .ok()
            .map(|v| {
                crate::monitor::ThresholdConfig::default().classify(v.used_percent(), v.free_bytes)
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
fn process_snapshot(identities: &[&Identity]) -> io::Result<BTreeMap<u32, Identity>> {
    if identities.is_empty() {
        return Ok(BTreeMap::new());
    }
    let pids = identities
        .iter()
        .map(|i| i.pid.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let outcome = run_with_timeout_retrying(
        || {
            let mut c = Command::new("/bin/ps");
            c.args(["-p", &pids, "-o", "pid=,lstart=,comm="])
                .env("LC_ALL", "C")
                .env("TZ", "UTC");
            c
        },
        LIVENESS_PROBE_TIMEOUT,
        LIVENESS_PROBE_RETRY_BUDGET,
    );
    let result = match outcome {
        CommandOutcome::Completed(o) if o.status.success() || o.status.code() == Some(1) => {
            String::from_utf8(o.stdout).map_err(error)?
        }
        _ => return Err(error("owner snapshot failed")),
    };
    let boot = boot_identity()?;
    let mut map = BTreeMap::new();
    for line in result.lines() {
        let w: Vec<_> = line.split_whitespace().collect();
        if w.len() < 7 {
            return Err(error("owner snapshot incomplete"));
        }
        let pid = w[0].parse().map_err(error)?;
        map.insert(
            pid,
            Identity {
                pid,
                start: w[1..6].join(" "),
                executable: w[6..].join(" "),
                boot: boot.clone(),
            },
        );
    }
    if identities.iter().any(|i| !map.contains_key(&i.pid)) {
        let outcome = run_with_timeout_retrying(
            || {
                let mut c = Command::new("/bin/ps");
                c.args(["-axo", "pid="]);
                c
            },
            LIVENESS_PROBE_TIMEOUT,
            LIVENESS_PROBE_RETRY_BUDGET,
        );
        let all = match outcome {
            CommandOutcome::Completed(o) if o.status.success() => {
                String::from_utf8(o.stdout).map_err(error)?
            }
            _ => return Err(error("owner absence unverified")),
        };
        for i in identities.iter().filter(|i| !map.contains_key(&i.pid)) {
            if all.split_whitespace().any(|s| s == i.pid.to_string()) {
                return Err(error("owner temporarily unreachable"));
            }
        }
    }
    Ok(map)
}
fn compare_snapshot(path: &Path, snapshot: &Path) -> io::Result<()> {
    secure(snapshot, false)?;
    let expected = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(snapshot)?;
    let mut bytes = vec![];
    expected.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(error("expected native configuration too large"));
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound && bytes.is_empty() => Ok(()),
        Err(e) => Err(e),
        Ok(_) => {
            secure(path, false)?;
            if bytes.is_empty() {
                return Err(error("empty expected snapshot means absent native state"));
            }
            let mut actual = vec![];
            OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(path)?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut actual)?;
            if actual.len() > 1024 * 1024 {
                return Err(error("current native configuration too large"));
            }
            if actual == bytes {
                Ok(())
            } else {
                Err(error("native configuration changed; stale write refused"))
            }
        }
    }
}
fn durable_raw(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("native configuration too large"));
    }
    if path.exists() {
        secure(path, false)?;
    }
    let temp = path.with_extension(format!("tmp-{}", random()?));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&temp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(
        path.parent()
            .ok_or_else(|| error("native parent unavailable"))?,
    )?
    .sync_all()?;
    Ok(())
}
fn authority(args: &[String]) -> io::Result<()> {
    let _lock = crate::executor::lock::acquire_execution_lock().map_err(lock_error)?;
    let target = crate::autopilot::store::default_envelope_path()?;
    let parent = target
        .parent()
        .ok_or_else(|| error("authority parent missing"))?;
    if !parent.exists() {
        private_dir(parent)?;
    }
    match args.first().map(String::as_str){
  Some("apply") if args.len()==4 && args[2]=="--expected-current"=>{
   compare_snapshot(&target,Path::new(&args[3]))?;
   let raw=fs::read(&args[1])?;
   let envelope=if raw.is_empty(){crate::autopilot::AutopilotEnvelope::revoked()}else{crate::autopilot::store::load_envelope_at(Path::new(&args[1])).map_err(error)?};
   let bytes=if raw.is_empty(){crate::autopilot::store::render_envelope(&envelope).into_bytes()}else{raw};durable_raw(&target,&bytes)?;
  },
  Some("grant-managed") if args.len()==9=>{
   let mut flags=BTreeMap::new();for pair in args[1..].as_chunks::<2>().0{if flags.insert(pair[0].as_str(),pair[1].as_str()).is_some(){return Err(error("duplicate grant field"));}}
   if flags.len()!=4 || !flags.contains_key("--expected-current"){return Err(error("managed grant requires explicit snapshot/budgets"));}
   compare_snapshot(&target,Path::new(flags["--expected-current"]))?;
   let prior=crate::autopilot::load_envelope().map_err(error)?;
   if prior.is_enabled() && (prior.allowed_kinds().iter().any(|k|*k!=crate::evidence::ResourceKind::ManagedGenerated) || !prior.ask_preauthorizations().is_empty()){return Err(error("unrelated standing authority preserved; separate owner decision required"));}
   let mut grant=crate::autopilot::AutopilotEnvelope::default();grant.enable();grant.allow_kind(crate::evidence::ResourceKind::ManagedGenerated).map_err(error)?;
   grant.set_max_actions(flags.get("--max-actions").ok_or_else(||error("action budget required"))?.parse().map_err(error)?).map_err(error)?;
   grant.set_max_bytes(flags.get("--max-bytes").ok_or_else(||error("byte budget required"))?.parse().map_err(error)?).map_err(error)?;
   grant.set_max_duration(Duration::from_secs(flags.get("--max-duration").ok_or_else(||error("duration budget required"))?.parse().map_err(error)?)).map_err(error)?;
   grant.set_min_pressure(prior.min_pressure());grant.set_respond_to_alerts(true);durable_raw(&target,crate::autopilot::store::render_envelope(&grant).as_bytes())?;
  },_=>return Err(error("authority apply FILE --expected-current SNAPSHOT | grant-managed --expected-current SNAPSHOT --max-actions N --max-bytes N --max-duration SECS"))
 }
    write_reply(&crate::autopilot::envelope_report(
        &crate::autopilot::load_envelope().map_err(error)?,
        Some(target.display().to_string()),
    ))
}

#[cfg(test)]
mod audit_outcome_tests {
    use super::*;

    fn reclaimed_item() -> SweepItem {
        SweepItem {
            id: "abc".into(),
            lifecycle: Lifecycle::Reclaimed,
            status: "reclaimed".into(),
            blockers: vec![],
            logical_bytes_removed: 1,
            physical_free_delta: None,
        }
    }

    fn io_err() -> io::Error {
        error("probe failed")
    }

    /// HORO-1920 (review of #186): the close decision comes from what is on disk,
    /// not from a `reclaimed` flag the ownership pass may not have reached. A
    /// resource that is gone is `succeeded` whatever any record says; one that
    /// is still there is `interrupted`; anything unprovable stays open.
    #[test]
    fn audit_close_outcome_is_decided_by_presence_and_leaves_the_unprovable_open() {
        assert_eq!(audit_close_outcome(true, Ok(true)), Some("succeeded"));
        assert_eq!(audit_close_outcome(true, Ok(false)), Some("interrupted"));
        assert_eq!(audit_close_outcome(true, Err(io_err())), None);
        assert_eq!(audit_close_outcome(false, Ok(true)), None);
        assert_eq!(audit_close_outcome(false, Ok(false)), None);
    }

    /// HORO-1920 (review of #186): when the monitor runs the sweep, a refusal
    /// because the audit log is unwritable must reach the monitor log, or it is
    /// indistinguishable from "nothing was eligible".
    #[test]
    fn a_refusal_for_an_unwritable_audit_log_reaches_the_monitor_log() {
        let refused = SweepItem {
            id: "r1".into(),
            lifecycle: Lifecycle::Reclaimable,
            status: "preserved".into(),
            blockers: vec![
                "audit log not writable (permission denied); nothing was deleted".into(),
            ],
            logical_bytes_removed: 0,
            physical_free_delta: None,
        };
        let quiet = SweepItem {
            blockers: vec!["meaningful-use inactivity threshold not reached".into()],
            id: "r2".into(),
            status: "preserved".into(),
            ..reclaimed_item()
        };
        let lines = sweep_log_lines(&[refused, quiet, reclaimed_item()]);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("r1") && lines[0].contains("audit log is not writable"));
        assert!(lines[1].contains("abc") && lines[1].contains("reclaimed"));
    }

    /// HORO-1920: a deletion whose outcome could not be recorded is reported,
    /// not presented as a clean success. The deletion itself already happened,
    /// so the status stays `reclaimed`; the blocker is what makes the missing
    /// record explicit, and the log still holds `attempted` for reconcile.
    #[test]
    fn an_unrecorded_outcome_is_surfaced_not_swallowed() {
        let mut item = reclaimed_item();
        note_outcome_audit(&mut item, Err(error("disk full")));
        assert_eq!(item.status, "reclaimed");
        assert_eq!(item.blockers.len(), 1);
        assert!(item.blockers[0].contains("audit outcome not recorded"));
        assert!(item.blockers[0].contains("reconciliation will complete it"));

        let mut clean = reclaimed_item();
        note_outcome_audit(&mut clean, Ok(()));
        assert!(clean.blockers.is_empty());
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    #[test]
    fn cursor_visits_beyond_sixty_four_and_wraps_without_mutating_discovery() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "glomeris-cursor-{}-{}",
            std::process::id(),
            random().unwrap()
        ));
        let store = Store { root: root.clone() };
        store.init().unwrap();
        for n in 0..130 {
            let id = format!("{n:032x}");
            fs::write(store.record_path(&id).unwrap(), b"{}").unwrap();
        }
        let lock =
            crate::executor::lock::acquire_execution_lock_at(&root.join("execution.lock")).unwrap();
        let a = store.batch_ids("sweep").unwrap();
        assert_eq!(a.len(), 64);
        assert!(!store.cursor_path("sweep").unwrap().exists());
        store
            .advance_cursor("sweep", a.last().unwrap(), &lock)
            .unwrap();
        let b = store.batch_ids("sweep").unwrap();
        assert_eq!(b.len(), 64);
        assert!(a.iter().all(|id| !b.contains(id)));
        store
            .advance_cursor("sweep", b.last().unwrap(), &lock)
            .unwrap();
        let c = store.batch_ids("sweep").unwrap();
        assert_eq!(c[0], format!("{:032x}", 128));
        assert_eq!(c[2], a[0]);
        assert_eq!(
            store.batch_ids("reconcile").unwrap(),
            a,
            "independent phases must not starve each other"
        );
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod record_warning_tests {
    use super::*;
    use crate::executor::{settle_record_result, ExecutionOutcome};

    fn denied() -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }

    #[test]
    fn a_successful_delete_stays_succeeded_when_the_record_cannot_be_saved() {
        let (mut outcome, mut warning) = (ExecutionOutcome::Succeeded, None);
        settle_record_result(&mut outcome, &mut warning, denied());
        assert_eq!(outcome, ExecutionOutcome::Succeeded);
        assert!(warning.unwrap().contains("persistence failed"));
    }

    #[test]
    fn a_failed_run_reports_the_persistence_failure_as_before() {
        let (mut outcome, mut warning) = (ExecutionOutcome::Failed("boom".into()), None);
        settle_record_result(&mut outcome, &mut warning, denied());
        assert!(
            matches!(&outcome, ExecutionOutcome::Failed(m) if m.contains("persistence failed"))
        );
        assert!(warning.is_none());
    }

    #[test]
    fn a_saved_record_changes_nothing() {
        let (mut outcome, mut warning) = (ExecutionOutcome::Succeeded, None);
        settle_record_result(&mut outcome, &mut warning, Ok(()));
        assert_eq!(outcome, ExecutionOutcome::Succeeded);
        assert!(warning.is_none());
    }

    /// The real sequence: the generation is deleted, then the record directory
    /// refuses the write that `finish` makes. Without the fix this is `Failed`.
    #[test]
    fn a_real_delete_followed_by_a_refused_record_write_is_still_succeeded() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("glomeris-record-warning-{}", random().unwrap()));
        let store = Store { root: root.clone() };
        store.init().unwrap();
        let id = "00000000000000000000000000000001";
        let record = Record {
            schema: 1,
            id: id.into(),
            generation: id.into(),
            class: "managed_generated".into(),
            recipe: "fixture".into(),
            source_root: root.clone(),
            source_head: "0".repeat(40),
            dev: 1,
            ino: 2,
            last_meaningful_use: 0,
            claims: vec![],
            read_claims: vec![],
            sealed: None,
            producer: None,
            production_started: true,
            retirement_started: true,
            retirement_history: vec![],
            reclaimed: false,
            observed_free_delta: None,
        };
        store.save(&record).unwrap();
        let data = store.path(&record);
        fs::create_dir_all(&data).unwrap();
        // The ownership checks reject anything not private, which would fail
        // `finish` for a reason other than the one under test.
        for dir in [data.parent().unwrap(), data.as_path()] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(data.join("output.o"), b"object").unwrap();
        let lock =
            crate::executor::lock::acquire_execution_lock_at(&root.join("execution.lock")).unwrap();
        let mut guard = RetirementGuard {
            _barrier: store.barrier(id, true).unwrap(),
            store: Store { root: root.clone() },
            record: record.clone(),
        };

        fs::remove_dir_all(&data).unwrap();
        assert!(!data.exists(), "the delete really happened");
        let records = root.join("records");
        fs::set_permissions(&records, fs::Permissions::from_mode(0o500)).unwrap();
        // Root ignores directory modes, so the write cannot be refused there.
        let refusable = File::create(records.join("probe")).is_err();
        let saved = if refusable {
            guard.finish(None)
        } else {
            Ok(())
        };
        fs::set_permissions(&records, fs::Permissions::from_mode(0o700)).unwrap();
        if !refusable {
            let _ = fs::remove_file(records.join("probe"));
            drop(lock);
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert_eq!(
            saved.as_ref().err().map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied),
            "the record write must have been refused by the directory mode"
        );

        let (mut outcome, mut warning) = (ExecutionOutcome::Succeeded, None);
        settle_record_result(&mut outcome, &mut warning, saved);
        assert_eq!(outcome, ExecutionOutcome::Succeeded);
        assert!(warning.is_some());
        assert!(
            !store.read(id).unwrap().reclaimed,
            "the record is left for reconcile to settle"
        );
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod permit_tests {
    use super::*;
    #[test]
    fn single_use_permit_rejects_each_binding_mismatch() {
        use crate::evidence::{ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator};
        use crate::policy::{PolicyClass, PolicyDecision, ReasonCode};
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("glomeris-permit-{}", random().unwrap()));
        let store = Store { root: root.clone() };
        store.init().unwrap();
        let lock =
            crate::executor::lock::acquire_execution_lock_at(&root.join("execution.lock")).unwrap();
        let id = "00000000000000000000000000000001";
        let artifact = Artifact {
            dev: 1,
            ino: 2,
            len: 10,
            mtime: 0,
            mtime_nsec: 0,
        };
        let record = Record {
            schema: 1,
            id: id.into(),
            generation: id.into(),
            class: "managed_generated".into(),
            recipe: "fixture".into(),
            source_root: root.clone(),
            source_head: "0".repeat(40),
            dev: 1,
            ino: 2,
            last_meaningful_use: 0,
            claims: vec![],
            read_claims: vec![],
            sealed: None,
            producer: Some(ProducerProof {
                input: root.join("input.rs"),
                input_metadata: artifact.clone(),
                compiler_version: "fixture".into(),
                outputs: BTreeMap::from([("output.o".into(), artifact)]),
                closed_consumers: true,
            }),
            production_started: true,
            retirement_started: false,
            retirement_history: vec![],
            reclaimed: false,
            observed_free_delta: None,
        };
        let resource = ResourceId::new(
            ResourceKind::ManagedGenerated,
            ResourceLocator::Path(store.path(&record)),
        );
        let fingerprint = ResourceFingerprint {
            dev_ino: Some((1, 2)),
            mtime: None,
            tool_revision: None,
        };
        let approval = crate::policy::approval::authorize(
            PolicyDecision {
                resource: resource.clone(),
                class: PolicyClass::AutoSafe,
                reasons: vec![ReasonCode::EvidenceFreshAndComplete],
                evidence_collected_at: UNIX_EPOCH,
                evaluated_at: UNIX_EPOCH,
                policy_version: 1,
            },
            fingerprint.clone(),
            None,
        )
        .unwrap();
        for mismatch in [
            "resource",
            "generation",
            "fingerprint",
            "action",
            "bytes",
            "matching",
        ] {
            let guard = RetirementGuard {
                _barrier: store.barrier(id, true).unwrap(),
                store: Store { root: root.clone() },
                record: record.clone(),
            };
            let mut permit = ManagedRetirementPermit {
                resource: resource.clone(),
                generation: record.generation.clone(),
                fingerprint: fingerprint.clone(),
                action: crate::actions::ActionId("managed.clean.generated"),
                admitted_bytes: 10,
                guard,
                _authority: &lock,
            };
            match mismatch {
                "resource" => {
                    permit.resource = ResourceId::new(
                        ResourceKind::ManagedGenerated,
                        ResourceLocator::Path(root.join("other")),
                    )
                }
                "generation" => permit.generation = "0".repeat(32),
                "fingerprint" => permit.fingerprint.dev_ino = Some((3, 4)),
                "action" => permit.action = crate::actions::ActionId("other"),
                "bytes" => permit.admitted_bytes = 9,
                _ => {}
            }
            assert_eq!(
                permit
                    .into_guard(&crate::actions::ManagedGeneratedClean, &approval)
                    .is_ok(),
                mismatch == "matching",
                "{mismatch}"
            );
        }
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
