//! Real-process, isolated-home acceptance probes. Never operate on workstation resources.
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
static FIXTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[test]
fn core_export_releases_source_access_without_native_exposure() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    let destination = f.home.join("exported.o");
    let result = f.call(&[
        "export",
        r["id"].as_str().unwrap(),
        r["generation"].as_str().unwrap(),
        "--artifact",
        "artifact.o",
        "--destination",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(result["status"], "exported");
    let source = PathBuf::from(r["path"].as_str().unwrap()).join("artifact.o");
    assert_eq!(fs::read(&source).unwrap(), fs::read(&destination).unwrap());
    assert_ne!(
        source.metadata().unwrap().ino(),
        destination.metadata().unwrap().ino()
    );
    let record: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    assert_eq!(record["producer"]["closed_consumers"], true);
    assert_eq!(record["read_claims"][0]["ended"], true);
    assert_eq!(record["read_claims"][0]["exported"], true);
    assert_eq!(f.inspect()[0]["eligible"], true);
    let sweep = f.call(&["sweep", "--unattended"]);
    assert_eq!(sweep[0]["status"], "reclaimed");
    assert!(
        destination.exists(),
        "export belongs to its owner, not this cleanup authority"
    );
    assert_eq!(
        f.call(&["sweep", "--unattended"])[0]["logical_bytes_removed"],
        0
    );
}

#[test]
fn export_refuses_overwrite_and_reconciles_ended_access_not_success() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    let destination = f.home.join("unique.o");
    fs::write(&destination, "unique user-created contents").unwrap();
    let result = f
        .cmd()
        .args([
            "resource",
            "export",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
            "--artifact",
            "artifact.o",
            "--destination",
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "unique user-created contents"
    );
    assert_eq!(f.inspect()[0]["eligible"], false);
    f.call(&["reconcile"]);
    let record: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    assert_eq!(record["read_claims"][0]["ended"], true);
    assert_eq!(record["read_claims"][0]["exported"], false);
    assert_eq!(f.inspect()[0]["eligible"], true);
}

#[test]
fn export_refuses_redirected_destination_and_unknown_artifact() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let r = f.create();
    f.object(&r);
    symlink(&f.source, f.home.join("redirect")).unwrap();
    for (artifact, destination) in [
        ("artifact.o", f.home.join("redirect/copy.o")),
        ("../artifact.o", f.home.join("copy.o")),
        ("unregistered.o", f.home.join("copy.o")),
        (
            "artifact.o",
            PathBuf::from(r["path"].as_str().unwrap()).join("copy.o"),
        ),
    ] {
        let result = f
            .cmd()
            .args([
                "resource",
                "export",
                r["id"].as_str().unwrap(),
                r["generation"].as_str().unwrap(),
                "--artifact",
                artifact,
                "--destination",
                destination.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!destination.exists());
    }
}

#[test]
fn export_cannot_restore_closure_after_native_hold() {
    let f = Fixture::new();
    let r = f.create();
    f.object(&r);
    f.holder(&r, "claude", std::process::id()).release();
    let destination = f.home.join("copy.o");
    let result = f
        .cmd()
        .args([
            "resource",
            "export",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
            "--artifact",
            "artifact.o",
            "--destination",
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!destination.exists());
}
struct Fixture {
    home: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let name = format!(
            "glomeris-managed-{}-{}-{}",
            std::process::id(),
            FIXTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let home = std::env::temp_dir().join(name);
        fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let source = home.join("source");
        fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        fs::write(
            source.join("lib.rs"),
            "#[no_mangle] pub static ARTIFACT: [u8; 1048576] = [42;1048576];\n",
        )
        .unwrap();
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\nedition='2021'\n",
        )
        .unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["add", "Cargo.toml", "lib.rs"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["commit", "-qm", "fixture"])
            .status()
            .unwrap()
            .success());
        Self { home, source }
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_glomeris"));
        c.env("HOME", &self.home).current_dir(&self.source);
        c
    }
    fn call(&self, args: &[&str]) -> Value {
        let o = self.cmd().arg("resource").args(args).output().unwrap();
        assert!(
            o.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    }
    /// `resource inspect`, retried while the owner-liveness probe is inconclusive.
    ///
    /// HORO-2013. `inspect` shells out to `ps` and, by design (HORO-1914), reports
    /// `UNKNOWN` and withholds eligibility when that probe fails, rather than
    /// guessing the owner is gone. On a loaded host the probe can fail
    /// transiently, which is the product behaving correctly, but the tests here
    /// assert the settled lifecycle. Retry only that exact inconclusive case; a
    /// persistent failure still reaches the caller's assertion unchanged.
    fn inspect(&self) -> Value {
        let mut last = self.call(&["inspect"]);
        for _ in 0..10 {
            let inconclusive = last.as_array().is_some_and(|rows| {
                rows.iter().any(|r| {
                    r["lifecycle"] == "UNKNOWN"
                        && r["blockers"].as_array().is_some_and(|b| {
                            b.iter().any(|m| {
                                m.as_str()
                                    .is_some_and(|s| s.contains("liveness could not be confirmed"))
                            })
                        })
                })
            });
            if !inconclusive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            last = self.call(&["inspect"]);
        }
        last
    }
    fn create(&self) -> Value {
        self.call(&[
            "create",
            "--source-root",
            self.source.to_str().unwrap(),
            "--recipe",
            "cargo build --locked (fixture compiler objects)",
        ])
    }
    fn policy(&self, threshold: u64, enabled: bool) {
        let p = self.home.join("policy.json");
        fs::write(&p,json!({"schema":1,"classes":{"managed_generated":{"idle_threshold_secs":threshold,"unattended":enabled}},"max_actions":3,"max_bytes":5368709120u64}).to_string()).unwrap();
        self.call(&["policy", "apply", p.to_str().unwrap()]);
        if enabled {
            let o = self
                .cmd()
                .args([
                    "autopilot",
                    "enable",
                    "--kinds",
                    "managed_generated",
                    "--respond-to-alerts",
                ])
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }
    fn object(&self, r: &Value) {
        self.call(&[
            "produce",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
            "--input",
            "lib.rs",
        ]);
    }
    fn seal(&self, r: &Value) {
        self.call(&[
            "seal",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
        ]);
    }
    fn holder(&self, r: &Value, host: &str, owner: u32) -> Holder {
        let mut child = self
            .cmd()
            .args([
                "resource",
                "hold",
                r["id"].as_str().unwrap(),
                r["generation"].as_str().unwrap(),
                "--host",
                host,
                "--session",
                "fixture-session",
                "--owner-pid",
                &owner.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut l = String::new();
        reader.read_line(&mut l).unwrap();
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["status"], "held");
        Holder { child, reader }
    }
    fn audit_path(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/Glomeris/actions.jsonl")
    }
    fn audit_lines(&self) -> Vec<Value> {
        fs::read_to_string(self.audit_path())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn record(&self, r: &Value) -> PathBuf {
        self.home
            .join("Library/Application Support/Glomeris/resources/records")
            .join(format!("{}.json", r["id"].as_str().unwrap()))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}
struct Holder {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}
impl Holder {
    fn op(&mut self, value: Value) -> Value {
        let pipe = self.child.stdin.as_mut().unwrap();
        writeln!(pipe, "{value}").unwrap();
        pipe.flush().unwrap();
        let mut l = String::new();
        self.reader.read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }
    fn release(mut self) {
        assert_eq!(self.op(json!({"op":"release"}))["status"], "ok");
        self.child.wait().unwrap();
    }
}
impl Drop for Holder {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.stdin.take();
            self.child.wait().unwrap();
        }
    }
}
#[test]
fn default_is24hours_not_creation_age() {
    let f = Fixture::new();
    let p = f.call(&["policy", "show"]);
    assert_eq!(
        p["classes"]["managed_generated"]["idle_threshold_secs"],
        86400
    );
    assert_eq!(p["classes"]["managed_generated"]["unattended"], false);
}
#[test]
fn positive_unknown_owner_generated_manifest_reclaims_once() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let v = f.call(&["sweep", "--unattended"]);
    assert_eq!(v[0]["status"], "reclaimed", "{v}");
    assert!(v[0]["logical_bytes_removed"].as_u64().unwrap() >= 1048576);
    assert!(!PathBuf::from(r["path"].as_str().unwrap()).exists());
    let second = f.call(&["sweep", "--unattended"]);
    assert_eq!(second[0]["logical_bytes_removed"], 0);
    assert_eq!(second[0]["lifecycle"], "RECLAIMED");
}
// HORO-1920: the sweep is the one deleting path that bypassed `actions.jsonl`,
// so a deletion the monitor made while nobody was looking was invisible to
// `glomeris actions history` and to the History panel. The log brackets every
// deletion: a durable `attempted` intent before it, the outcome after it.
#[test]
fn managed_sweep_reclamation_is_bracketed_in_the_audit_log() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let v = f.call(&["sweep", "--unattended"]);
    assert_eq!(v[0]["status"], "reclaimed", "{v}");
    let lines = f.audit_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "attempted");
    assert_eq!(lines[1]["outcome"], "succeeded");
    for line in &lines {
        assert_eq!(line["source"], "managed_sweep");
        assert_eq!(line["action_id"], "managed.clean.generated");
        assert_eq!(line["policy_label"], "AUTO_SAFE");
    }
    assert_eq!(lines[0]["resource_id"], lines[1]["resource_id"]);
    assert!(lines[1]["actual_reclaimed_bytes"].as_u64().unwrap() >= 1048576);
    assert!(lines[0]["timestamp"].as_u64() <= lines[1]["timestamp"].as_u64());
    let second = f.call(&["sweep", "--unattended"]);
    assert_eq!(second[0]["logical_bytes_removed"], 0);
    assert_eq!(
        f.audit_lines().len(),
        2,
        "a sweep that deletes nothing must not write a record"
    );
}

// HORO-1920: if the log cannot be written, nothing is deleted.
#[test]
fn managed_sweep_refuses_to_delete_when_the_audit_log_is_unwritable() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    // A directory where the log should be makes every open for append fail.
    fs::create_dir_all(f.audit_path()).unwrap();
    let v = f.call(&["sweep", "--unattended"]);
    assert_eq!(v[0]["status"], "preserved", "{v}");
    assert!(
        v[0]["blockers"]
            .to_string()
            .contains("audit log not writable"),
        "{v}"
    );
    assert!(
        PathBuf::from(r["path"].as_str().unwrap()).exists(),
        "an unauditable deletion must not happen"
    );
    let record: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    assert_eq!(record["retirement_started"], false);
    assert_eq!(record["reclaimed"], false);
    // Once the log is writable again the same generation is reclaimed normally.
    fs::remove_dir_all(f.audit_path()).unwrap();
    let again = f.call(&["sweep", "--unattended"]);
    assert_eq!(again[0]["status"], "reclaimed", "{again}");
}
#[test]
fn possible_unique_data_and_unsealed_generation_preserve() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    fs::write(
        PathBuf::from(r["path"].as_str().unwrap()).join("notes.txt"),
        "unique",
    )
    .unwrap();
    let o = f
        .cmd()
        .args([
            "resource",
            "seal",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let v = f.call(&["sweep", "--unattended"]);
    assert_eq!(v[0]["status"], "preserved");
    assert!(PathBuf::from(r["path"].as_str().unwrap())
        .join("notes.txt")
        .exists());
}
#[test]
fn two_hosts_live_idle_and_renewal_never_authorize_cleanup() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    let mut a = f.holder(&r, "claude", std::process::id());
    let b = f.holder(&r, "codex", std::process::id());
    assert_eq!(a.op(json!({"op":"progress","sequence":1}))["status"], "ok");
    let before: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    assert_eq!(a.op(json!({"op":"renew"}))["status"], "ok");
    let after: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    assert_eq!(before["last_meaningful_use"], after["last_meaningful_use"]);
    a.release();
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "preserved");
    b.release();
    let seal = f
        .cmd()
        .args([
            "resource",
            "seal",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !seal.status.success(),
        "native exposure cannot renew closure proof"
    );
    assert_ne!(f.inspect()[0]["lifecycle"], "RECLAIMABLE");
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
}
#[test]
fn long_running_progress_and_silent_valid_owner_are_protected() {
    let f = Fixture::new();
    f.policy(86400, true);
    let r = f.create();
    let mut holder = f.holder(&r, "codex", std::process::id());
    let mut stored: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    stored["last_meaningful_use"] = json!(1);
    fs::write(f.record(&r), stored.to_string()).unwrap();
    assert_eq!(
        holder.op(json!({"op":"progress","sequence":1}))["status"],
        "ok"
    );
    let v = f.inspect();
    assert_eq!(v[0]["lifecycle"], "ACTIVE");
    assert!(!v[0]["eligible"].as_bool().unwrap());
    assert_eq!(
        holder.op(json!({"op":"progress","sequence":1}))["status"],
        "refused"
    );
    holder.release();
}
#[test]
fn stronger_stalled_health_is_diagnostic_not_termination() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    let mut h = f.holder(&r, "claude", std::process::id());
    assert_eq!(
        h.op(json!({"op":"health","faults":["cpu_high"]}))["status"],
        "refused"
    );
    assert_eq!(
        h.op(json!({"op":"health","faults":["checkpoint_stale","dependency_failed"]}))["status"],
        "ok"
    );
    assert_eq!(f.inspect()[0]["lifecycle"], "STALLED");
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "preserved");
    h.release();
}
#[test]
fn late_unique_data_and_path_generation_changes_refuse() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    fs::write(
        PathBuf::from(r["path"].as_str().unwrap()).join("new.rs"),
        "unique",
    )
    .unwrap();
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "preserved");
    let p = PathBuf::from(r["path"].as_str().unwrap());
    fs::rename(&p, p.with_extension("kept")).unwrap();
    fs::create_dir(&p).unwrap();
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "preserved");
}
#[test]
fn crash_pending_child_is_unknown_not_auto_release() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    let mut h = f.holder(&r, "codex", std::process::id());
    assert_eq!(h.op(json!({"op":"child_pending"}))["status"], "ok");
    assert_eq!(h.op(json!({"op":"release"}))["status"], "refused");
    drop(h);
    let view = f.call(&["reconcile"]);
    assert!(!view[0]["eligible"].as_bool().unwrap());
    assert!(view[0]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str().unwrap().contains("unconfirmed child")));
}
#[test]
fn corrupt_state_and_revoked_policy_refuse_without_deletion() {
    let f = Fixture::new();
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    f.policy(0, false);
    assert_eq!(f.call(&["sweep", "--unattended"]), json!([]));
    fs::write(f.record(&r), "broken").unwrap();
    assert!(!f
        .cmd()
        .args(["resource", "inspect"])
        .status()
        .unwrap()
        .success());
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
}
#[test]
fn fabricated_object_header_is_not_producer_provenance() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    fs::write(
        PathBuf::from(r["path"].as_str().unwrap()).join("unique.o"),
        [0xcf, 0xfa, 0xed, 0xfe, 1, 2, 3, 4],
    )
    .unwrap();
    let o = f
        .cmd()
        .args([
            "resource",
            "seal",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "preserved");
}
#[test]
fn exec_transition_preserves_same_process_generation() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    let h = f.holder(&r, "worker", std::process::id());
    let mut rec: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    rec["claims"][0]["holder"]["executable"] = json!("previous-image-before-exec");
    rec["claims"][0]["owner"]["executable"] = json!("previous-image-before-exec");
    fs::write(f.record(&r), rec.to_string()).unwrap();
    let v = f.inspect();
    assert_eq!(v[0]["lifecycle"], "IDLE");
    assert!(!v[0]["eligible"].as_bool().unwrap());
    h.release();
}
#[test]
fn existing_autopilot_revocation_prevents_managed_sweep() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    assert!(f
        .cmd()
        .args(["autopilot", "revoke"])
        .status()
        .unwrap()
        .success());
    assert_eq!(f.call(&["sweep", "--unattended"]), json!([]));
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
}
#[test]
fn concurrent_hosts_sweep_without_double_deletion() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let a = f
        .cmd()
        .args(["resource", "sweep", "--unattended"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let b = f
        .cmd()
        .args(["resource", "sweep", "--unattended"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reclaimed = 0;
    for child in [a, b] {
        let o = child.wait_with_output().unwrap();
        if o.status.success() {
            let v: Value = serde_json::from_slice(&o.stdout).unwrap();
            reclaimed += v
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["status"] == "reclaimed")
                .count();
        } else {
            assert_eq!(o.status.code(), Some(75));
        }
    }
    assert_eq!(reclaimed, 1);
}
#[test]
fn interrupted_retirement_reconciles_absence_without_repeating_action() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let first = f.call(&["sweep", "--unattended"]);
    assert_eq!(first[0]["status"], "reclaimed");
    let mut stored: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    stored["reclaimed"] = json!(false);
    stored["retirement_started"] = json!(true);
    fs::write(f.record(&r), stored.to_string()).unwrap();
    assert_eq!(f.call(&["reconcile"])[0]["lifecycle"], "RECLAIMED");
    assert_eq!(
        f.call(&["sweep", "--unattended"])[0]["logical_bytes_removed"],
        0
    );
}
#[test]
fn dangling_replacement_is_not_verified_removal() {
    let f = Fixture::new();
    let r = f.create();
    let path = PathBuf::from(r["path"].as_str().unwrap());
    fs::remove_dir(&path).unwrap();
    std::os::unix::fs::symlink(f.home.join("absent"), &path).unwrap();
    let mut stored: Value = serde_json::from_slice(&fs::read(f.record(&r)).unwrap()).unwrap();
    stored["retirement_started"] = json!(true);
    fs::write(f.record(&r), stored.to_string()).unwrap();
    let v = f.call(&["reconcile"]);
    assert_ne!(v[0]["lifecycle"], "RECLAIMED");
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
}

#[test]
fn shared_kernel_barrier_blocks_otherwise_eligible_generation() {
    use nix::fcntl::{Flock, FlockArg};
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let barrier = f
        .home
        .join("Library/Application Support/Glomeris/resources/barriers")
        .join(format!("{}.lock", r["id"].as_str().unwrap()));
    let guard = Flock::lock(
        fs::File::open(barrier).unwrap(),
        FlockArg::LockSharedNonblock,
    )
    .unwrap();
    let rows = f.call(&["sweep", "--unattended"]);
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] != "reclaimed"),
        "{rows}"
    );
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
    drop(guard);
    assert_eq!(f.call(&["sweep", "--unattended"])[0]["status"], "reclaimed");
}
#[test]
fn pressure_floor_preserves_otherwise_eligible_generation() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let envelope = f
        .home
        .join("Library/Application Support/Glomeris/autopilot.conf");
    let old = fs::read_to_string(&envelope).unwrap();
    let changed = old
        .lines()
        .map(|s| {
            if s.starts_with("min_pressure =") {
                "min_pressure = EMERGENCY"
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(envelope, changed).unwrap();
    let out = f
        .cmd()
        .args(["resource", "sweep", "--unattended"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an emergency pressure floor must not admit a healthy fixture host"
    );
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
}
#[test]
fn producer_rejects_unretained_environment_dependency() {
    let f = Fixture::new();
    fs::write(
        f.source.join("lib.rs"),
        "pub const EXTERNAL: &str = env!(\"HOME\");\n",
    )
    .unwrap();
    assert!(Command::new("git")
        .arg("-C")
        .arg(&f.source)
        .args(["commit", "-qam", "external dependency"])
        .status()
        .unwrap()
        .success());
    let r = f.create();
    let out = f
        .cmd()
        .args([
            "resource",
            "produce",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
            "--input",
            "lib.rs",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let rows = f.inspect();
    assert_ne!(rows[0]["lifecycle"], "RECLAIMABLE");
}

#[test]
fn native_authority_compare_and_swap_refuses_drift_and_restores_absent_default() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let before = f.home.join("before.snapshot");
    fs::write(&before, b"").unwrap();
    fs::set_permissions(&before, fs::Permissions::from_mode(0o600)).unwrap();
    let grant = f.call(&[
        "authority",
        "grant-managed",
        "--expected-current",
        before.to_str().unwrap(),
        "--max-actions",
        "3",
        "--max-bytes",
        "5368709120",
        "--max-duration",
        "60",
    ]);
    assert_eq!(grant["enabled"], true);
    assert_eq!(grant["allowed_kinds"], json!(["managed_generated"]));
    let path = f
        .home
        .join("Library/Application Support/Glomeris/autopilot.conf");
    let installed = fs::read(&path).unwrap();
    let stale = f
        .cmd()
        .args([
            "resource",
            "authority",
            "apply",
            before.to_str().unwrap(),
            "--expected-current",
            before.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    assert_eq!(fs::read(&path).unwrap(), installed);
    let post = f.home.join("after.snapshot");
    fs::write(&post, &installed).unwrap();
    fs::set_permissions(&post, fs::Permissions::from_mode(0o600)).unwrap();
    let restored = f.call(&[
        "authority",
        "apply",
        before.to_str().unwrap(),
        "--expected-current",
        post.to_str().unwrap(),
    ]);
    assert_eq!(restored["enabled"], false);
    assert_eq!(restored["allowed_kinds"], json!([]));
}

#[test]
fn exposure_before_production_cannot_restore_closed_consumer_authority() {
    let f = Fixture::new();
    let r = f.create();
    let h = f.holder(&r, "claude", std::process::id());
    h.release();
    let out = f
        .cmd()
        .args([
            "resource",
            "produce",
            r["id"].as_str().unwrap(),
            r["generation"].as_str().unwrap(),
            "--input",
            "lib.rs",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!f.inspect()[0]["eligible"].as_bool().unwrap());
}

#[test]
fn generic_execution_and_free_cannot_bypass_managed_class_authority() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    f.policy(0, false); // envelope stays enabled; class authorization is withdrawn
    let path = r["path"].as_str().unwrap();
    let resource = format!("managed_generated:{path}");
    let direct = f
        .cmd()
        .args([
            "execute",
            "--action-id",
            "managed.clean.generated",
            "--resource-id",
            &resource,
            "--json",
        ])
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        PathBuf::from(path).exists(),
        "generic execution deleted managed data: {}",
        String::from_utf8_lossy(&direct.stdout)
    );
    let out = f
        .cmd()
        .args([
            "free",
            "--goal-used-percent",
            "60",
            "--autopilot",
            "--unattended",
            "--json",
        ])
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "free report: {} / {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(report["actions_executed"], 0, "{report}");
    assert_eq!(report["remaining"]["not_executable_count"], 1, "{report}");
    assert!(PathBuf::from(path).exists());
    assert_eq!(f.call(&["sweep", "--unattended"]), json!([]));
}
#[test]
fn class_byte_budget_preserves_oversized_real_generation() {
    let f = Fixture::new();
    f.policy(0, true);
    let r = f.create();
    f.object(&r);
    f.seal(&r);
    let policy = f.home.join("tiny-policy.json");
    fs::write(&policy,json!({"schema":1,"classes":{"managed_generated":{"idle_threshold_secs":0,"unattended":true}},"max_actions":3,"max_bytes":1}).to_string()).unwrap();
    f.call(&["policy", "apply", policy.to_str().unwrap()]);
    let rows = f.call(&["sweep", "--unattended"]);
    assert_eq!(rows[0]["logical_bytes_removed"], 0);
    assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
}
#[test]
fn cas_rejects_oversized_matching_prefix_with_different_suffix() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.policy(0, true);
    let path = f
        .home
        .join("Library/Application Support/Glomeris/autopilot.conf");
    let original = format!("#{}\\nversion = 1\\nenabled = true\\n", "x".repeat(1048576));
    fs::write(&path, original.as_bytes()).unwrap();
    let expected = f.home.join("oversized.snapshot");
    fs::write(
        &expected,
        original.replace("enabled = true", "enabled = false"),
    )
    .unwrap();
    fs::set_permissions(&expected, fs::Permissions::from_mode(0o600)).unwrap();
    let absent = f.home.join("absent.snapshot");
    fs::write(&absent, b"").unwrap();
    fs::set_permissions(&absent, fs::Permissions::from_mode(0o600)).unwrap();
    let out = f
        .cmd()
        .args([
            "resource",
            "authority",
            "apply",
            absent.to_str().unwrap(),
            "--expected-current",
            expected.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn corrupt_record_does_not_block_other_verified_reconciliation_and_cleanup() {
    let f = Fixture::new();
    f.policy(0, true);
    let first = f.create();
    let second = f.create();
    let (bad, good) = if first["id"].as_str().unwrap() < second["id"].as_str().unwrap() {
        (first, second)
    } else {
        (second, first)
    };
    f.object(&good);
    f.seal(&good);
    fs::write(f.record(&bad), b"invalid fixture record").unwrap();
    let reconciliation = f.call(&["reconcile"]);
    assert!(reconciliation
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == bad["id"] && row["lifecycle"] == "UNKNOWN"));
    let rows = f.call(&["sweep", "--unattended"]);
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == good["id"] && row["status"] == "reclaimed"));
    assert!(f.record(&bad).exists());
}
