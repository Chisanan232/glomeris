//! Kill owned child processes at real executor boundaries; never manufacture receipts.
use super::*;
use serde_json::{json, Value};
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

const DRIVER: &str = "ownership::crash_tests::child_process_driver";
const CONTROL_SOCKET: &str = "GLOMERIS_UNIT_CRASH_CONTROL_SOCKET";
const CRASH_PHASE: &str = "GLOMERIS_UNIT_CRASH_PHASE";
const WAIT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Checkpoint {
    IntentDurable,
    DeletedBeforeOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryCase {
    Unchanged,
    RevokedAuthority,
    ChangedContents,
}

impl Checkpoint {
    fn phase(self) -> &'static str {
        match self {
            Self::IntentDurable => "intent",
            Self::DeletedBeforeOutcome => "deleted",
        }
    }
}

pub(crate) fn checkpoint(phase: &str) {
    // This module and both callers exist only in the unit-test binary.
    if std::env::var(CRASH_PHASE).as_deref() != Ok(phase) {
        return;
    }
    println!("GLOMERIS_UNIT_CRASH_CHECKPOINT:{phase}");
    io::stdout().flush().expect("flush checkpoint marker");
    loop {
        std::thread::park();
    }
}

fn send(stream: &mut UnixStream, value: &Value) {
    writeln!(stream, "{value}").expect("write control message");
    stream.flush().expect("flush control message");
}

fn receive(stream: &mut BufReader<UnixStream>) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "control message deadline exceeded");
        stream.get_ref().set_read_timeout(Some(remaining)).unwrap();
        match stream.read_until(b'\n', &mut line) {
            Ok(0) => panic!("child closed control channel before reporting the required boundary"),
            Ok(_) if line.last() == Some(&b'\n') => {
                return serde_json::from_slice(&line).expect("decode control message");
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(e) => panic!("read control message: {e}"),
        }
    }
}

struct OwnedChild(Child, Receiver<String>);
impl OwnedChild {
    fn expect_checkpoint(&self, point: Checkpoint) {
        let marker = format!("GLOMERIS_UNIT_CRASH_CHECKPOINT:{}", point.phase());
        let deadline = Instant::now() + WAIT;
        loop {
            let line = self
                .1
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("real sweep must emit the required checkpoint before exiting");
            if line.ends_with(&marker) {
                return;
            }
        }
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.0.try_wait().expect("observe owned child") {
                return status;
            }
            assert!(Instant::now() < deadline, "owned child did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn interrupt(&mut self) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "child exited before interruption"
        );
        self.0.kill().expect("kill only the owned child");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(self.wait().signal(), Some(nix::libc::SIGKILL));
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Fixture {
    home: PathBuf,
    listener: UnixListener,
}
impl Fixture {
    fn new() -> Self {
        // Keep the socket below sockaddr_un's macOS path limit.
        let home = fs::canonicalize("/tmp").unwrap().join(format!(
            "gl-crash-{}-{}",
            std::process::id(),
            &random().unwrap()[..12]
        ));
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(home.join("control.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { home, listener }
    }
    fn store(&self) -> Store {
        Store {
            root: self
                .home
                .join("Library/Application Support/Glomeris/resources"),
        }
    }
    fn child(&self, request: Value) -> (OwnedChild, BufReader<UnixStream>) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", DRIVER, "--nocapture", "--test-threads=1"])
            .env("HOME", &self.home)
            .env(CONTROL_SOCKET, self.home.join("control.sock"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        if request["op"] == "interrupt" {
            let point: Checkpoint = serde_json::from_value(request["point"].clone()).unwrap();
            command.env(CRASH_PHASE, point.phase());
        } else {
            command.env_remove(CRASH_PHASE);
        }
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(name);
        }
        // Reuse the installed toolchain read-only under the isolated child HOME.
        if std::env::var_os("RUSTUP_HOME").is_none() {
            if let Some(home) = std::env::var_os("HOME") {
                command.env("RUSTUP_HOME", PathBuf::from(home).join(".rustup"));
            }
        }
        let mut process = command.spawn().unwrap();
        let stdout = process.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut child = OwnedChild(process, output);
        let deadline = Instant::now() + WAIT;
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        child.0.try_wait().unwrap().is_none(),
                        "child failed before connecting"
                    );
                    assert!(Instant::now() < deadline, "child never connected");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accept child: {e}"),
            }
        };
        // macOS accepts a socket with the listener's nonblocking flag.
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.set_write_timeout(Some(WAIT)).unwrap();
        send(&mut stream, &request);
        (child, BufReader::new(stream))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn git(source: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn prepare(store: &Store) -> Record {
    let source = PathBuf::from(std::env::var_os("HOME").unwrap()).join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("Cargo.toml"),
        "[package]\nname='crash_fixture'\nversion='0.0.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        source.join("lib.rs"),
        "#[no_mangle] pub static ARTIFACT: [u8; 1048576] = [42; 1048576];\n",
    )
    .unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.name", "Crash fixture"]);
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&source, &["config", "commit.gpgsign", "false"]);
    git(&source, &["add", "Cargo.toml", "lib.rs"]);
    git(&source, &["commit", "-qm", "retained reconstruction input"]);
    let mut policy = RetentionPolicy::default();
    let class = policy.classes.get_mut("managed_generated").unwrap();
    class.idle_threshold_secs = 0;
    class.unattended = true;
    store.apply_policy(&policy).unwrap();
    let mut envelope = crate::autopilot::AutopilotEnvelope::default();
    envelope.enable();
    envelope
        .allow_kind(crate::evidence::ResourceKind::ManagedGenerated)
        .unwrap();
    envelope.set_respond_to_alerts(true);
    crate::autopilot::save_envelope(&envelope).unwrap();
    let record = store
        .create(&source, "rustc --crate-type lib --emit=obj lib.rs")
        .unwrap();
    produce(store, &record.id, &record.generation, Path::new("lib.rs")).unwrap();
    let record = store.read(&record.id).unwrap();
    assert!(
        store.inspect(&record, now()).eligible,
        "fixture must independently qualify before interruption"
    );
    record
}

#[test]
fn child_process_driver() {
    let Some(socket) = std::env::var_os(CONTROL_SOCKET) else {
        return;
    };
    let stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let mut reader = BufReader::new(stream);
    let request = receive(&mut reader);
    let store = Store::current().unwrap();
    match request["op"].as_str().unwrap() {
        "interrupt" => {
            let point: Checkpoint = serde_json::from_value(request["point"].clone()).unwrap();
            let record = prepare(&store);
            send(
                reader.get_mut(),
                &json!({"event":"prepared", "id":record.id}),
            );
            let result = sweep(&store);
            // A missing hook or an earlier refusal must fail the parent, never count as protection.
            send(
                reader.get_mut(),
                &json!({"event":"missed_checkpoint", "result":format!("{result:?}")}),
            );
            panic!("real sweep never reached {point:?}");
        }
        "recover" => {
            let id = request["id"].as_str().unwrap();
            let point: Checkpoint = serde_json::from_value(request["point"].clone()).unwrap();
            let scenario: RecoveryCase =
                serde_json::from_value(request["scenario"].clone()).unwrap();
            if scenario == RecoveryCase::RevokedAuthority {
                let mut envelope = crate::autopilot::load_envelope().unwrap();
                envelope.revoke();
                crate::autopilot::save_envelope(&envelope).unwrap();
            }
            let before = store.read(id).unwrap();
            let artifact = store.path(&before).join("artifact.o");
            let bytes = match point {
                Checkpoint::IntentDurable => Some(fs::read(&artifact).unwrap()),
                Checkpoint::DeletedBeforeOutcome => {
                    assert!(verified_absent(&store.path(&before)).unwrap());
                    None
                }
            };
            let inspections = store.reconcile().unwrap();
            let reconciled = store.read(id).unwrap();
            if let Some(bytes) = bytes {
                assert_eq!(
                    fs::read(&artifact).unwrap(),
                    bytes,
                    "reconcile must preserve the whole artifact before any retry"
                );
            } else {
                assert!(verified_absent(&store.path(&reconciled)).unwrap());
            }
            let first = sweep(&store).unwrap();
            let second = sweep(&store).unwrap();
            send(
                reader.get_mut(),
                &json!({"event":"recovered", "inspections":inspections, "reconciled":reconciled, "first":first, "second":second}),
            );
        }
        other => panic!("unknown child request: {other}"),
    }
}

fn interrupted_retirement(point: Checkpoint, scenario: RecoveryCase) {
    let fixture = Fixture::new();
    let (mut child, mut control) = fixture.child(json!({"op":"interrupt", "point":point}));
    let prepared = receive(&mut control);
    assert_eq!(prepared["event"], "prepared", "{prepared}");
    let id = prepared["id"].as_str().unwrap();
    child.expect_checkpoint(point);
    let store = fixture.store();
    let global_lock = fixture
        .home
        .join("Library/Application Support/Glomeris/execution.lock");
    assert!(
        matches!(
            crate::executor::lock::acquire_execution_lock_at(&global_lock),
            Err(crate::executor::lock::LockError::AlreadyHeld)
        ),
        "retirement must retain the global lock at the interruption boundary"
    );
    assert_eq!(
        store
            .barrier(id, true)
            .expect_err("retirement must retain its exclusive barrier")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let before = store.read(id).unwrap();
    assert!(
        before.retirement_started,
        "intent must already be persisted"
    );
    assert!(!before.reclaimed, "outcome must not yet be persisted");
    let input = before.producer.as_ref().unwrap().input.clone();
    let retained_source = fs::read(&input).unwrap();
    assert!(!retained_source.is_empty());
    let artifact = store.path(&before).join("artifact.o");
    let mut expected_artifact = match point {
        Checkpoint::IntentDurable => {
            let bytes = fs::read(&artifact).expect("deletion must not yet have happened");
            assert!(bytes.len() >= 1048576);
            Some(bytes)
        }
        Checkpoint::DeletedBeforeOutcome => {
            assert!(
                verified_absent(&store.path(&before)).unwrap(),
                "real executor must already have deleted"
            );
            None
        }
    };
    child.interrupt();
    drop(control);
    let persisted = store.read(id).unwrap();
    assert!(persisted.retirement_started && !persisted.reclaimed);
    let original_record = serde_json::to_value(&persisted).unwrap();
    let history = original_record["retirement_history"]
        .as_array()
        .expect("the actual interrupted attempt must retain its history");
    assert_eq!(history.len(), 1);
    assert!(valid_id(history[0]["attempt_id"].as_str().unwrap()));
    assert_eq!(history[0]["generation"], original_record["generation"]);
    assert_eq!(history[0]["dev"], original_record["dev"]);
    assert_eq!(history[0]["ino"], original_record["ino"]);
    assert!(history[0]["intent_at"].as_u64().unwrap() > 0);
    assert!(history[0]["retry_queued_at"].is_null());
    assert!(history[0]["completed_at"].is_null());
    if scenario == RecoveryCase::ChangedContents {
        assert_eq!(point, Checkpoint::IntentDurable);
        let changed = expected_artifact.as_mut().unwrap();
        let extra = b"unique state written after interrupted retirement";
        let mut file = OpenOptions::new().append(true).open(&artifact).unwrap();
        file.write_all(extra).unwrap();
        file.sync_all().unwrap();
        changed.extend_from_slice(extra);
    }
    let (mut recovery, mut control) =
        fixture.child(json!({"op":"recover", "id":id, "point":point, "scenario":scenario}));
    let result = receive(&mut control);
    assert_eq!(result["event"], "recovered", "{result}");
    assert!(recovery.wait().success());
    let after = store.read(id).unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(
        fs::read(input).unwrap(),
        retained_source,
        "recovery source survives both crash windows"
    );
    let reconciled_history = result["reconciled"]["retirement_history"]
        .as_array()
        .unwrap();
    assert_eq!(reconciled_history.len(), 1);
    let final_record = serde_json::to_value(&after).unwrap();
    let final_history = final_record["retirement_history"].as_array().unwrap();
    for field in ["attempt_id", "generation", "dev", "ino", "intent_at"] {
        assert_eq!(reconciled_history[0][field], history[0][field], "{field}");
        assert_eq!(final_history[0][field], history[0][field], "{field}");
    }
    assert_eq!(final_history[0], reconciled_history[0]);
    match (point, scenario) {
        (Checkpoint::IntentDurable, RecoveryCase::Unchanged) => {
            assert_eq!(result["reconciled"]["retirement_started"], false);
            assert_eq!(result["reconciled"]["reclaimed"], false);
            assert_eq!(result["inspections"][0]["eligible"], true);
            assert_eq!(result["first"].as_array().unwrap().len(), 1);
            assert_eq!(result["first"][0]["status"], "reclaimed", "{result}");
            assert_eq!(
                result["first"][0]["logical_bytes_removed"]
                    .as_u64()
                    .unwrap(),
                expected_artifact.unwrap().len() as u64
            );
            assert!(after.reclaimed && !after.retirement_started);
            assert!(verified_absent(&store.path(&after)).unwrap());
            assert_eq!(final_history.len(), 2, "one original attempt and one retry");
            assert!(reconciled_history[0]["retry_queued_at"].is_u64());
            assert!(reconciled_history[0]["completed_at"].is_null());
            assert_ne!(final_history[1]["attempt_id"], history[0]["attempt_id"]);
            for field in ["generation", "dev", "ino"] {
                assert_eq!(final_history[1][field], history[0][field], "{field}");
            }
            assert!(final_history[1]["completed_at"].is_u64());
        }
        (Checkpoint::IntentDurable, RecoveryCase::RevokedAuthority) => {
            assert_eq!(result["reconciled"]["retirement_started"], false);
            assert_eq!(result["inspections"][0]["eligible"], true);
            assert!(!after.retirement_started && !after.reclaimed);
            assert_eq!(result["first"], json!([]), "{result}");
            assert_eq!(result["second"], json!([]), "{result}");
            assert_eq!(fs::read(&artifact).unwrap(), expected_artifact.unwrap());
            assert_eq!(after.observed_free_delta, None);
            assert_eq!(final_history.len(), 1, "revocation must prevent a retry");
            assert!(reconciled_history[0]["retry_queued_at"].is_u64());
            assert!(reconciled_history[0]["completed_at"].is_null());
            return;
        }
        (Checkpoint::IntentDurable, RecoveryCase::ChangedContents) => {
            assert!(
                after.retirement_started && !after.reclaimed,
                "changed producer contents must keep the intent protected"
            );
            assert_eq!(result["reconciled"]["retirement_started"], true);
            assert_eq!(fs::read(&artifact).unwrap(), expected_artifact.unwrap());
            assert_eq!(result["inspections"][0]["eligible"], false);
            assert_eq!(after.observed_free_delta, None);
            assert_eq!(final_history.len(), 1);
            assert_eq!(final_history, history);
        }
        (Checkpoint::DeletedBeforeOutcome, RecoveryCase::Unchanged) => {
            assert!(
                after.reclaimed && !after.retirement_started,
                "reconcile completes the absent generation"
            );
            assert_eq!(result["inspections"][0]["lifecycle"], "RECLAIMED");
            assert!(verified_absent(&store.path(&after)).unwrap());
            assert_eq!(
                after.observed_free_delta, None,
                "crashed measurement must not be invented"
            );
            assert_eq!(
                final_history.len(),
                1,
                "absence cannot trigger another attempt"
            );
            assert!(reconciled_history[0]["retry_queued_at"].is_null());
            assert!(reconciled_history[0]["completed_at"].is_u64());
        }
        _ => panic!("unsupported recovery scenario"),
    }
    for pass in ["first", "second"] {
        if pass == "first"
            && point == Checkpoint::IntentDurable
            && scenario == RecoveryCase::Unchanged
        {
            continue;
        }
        assert_eq!(result[pass].as_array().unwrap().len(), 1, "{result}");
        assert_eq!(
            result[pass][0]["logical_bytes_removed"], 0,
            "no additional deletion: {result}"
        );
        assert_ne!(result[pass][0]["status"], "reclaimed", "{result}");
    }
}

#[test]
fn kill_after_durable_intent_reconciles_then_retries_once() {
    interrupted_retirement(Checkpoint::IntentDurable, RecoveryCase::Unchanged);
}

#[test]
fn kill_after_deletion_reconciles_without_repeating_cleanup() {
    interrupted_retirement(Checkpoint::DeletedBeforeOutcome, RecoveryCase::Unchanged);
}

#[test]
fn revoked_authority_preserves_artifact_after_intent_reconciliation() {
    interrupted_retirement(Checkpoint::IntentDurable, RecoveryCase::RevokedAuthority);
}

#[test]
fn changed_contents_keep_interrupted_intent_protected() {
    interrupted_retirement(Checkpoint::IntentDurable, RecoveryCase::ChangedContents);
}
