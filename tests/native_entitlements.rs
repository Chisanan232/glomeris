//! Native entitlement CLI contract tests. All state and native resources live
//! in a private temporary fixture; no test contacts a Docker daemon.
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    home: PathBuf,
    scope: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "glomeris-native-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let home = PathBuf::from("/tmp").join(unique);
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let home = home.canonicalize().unwrap();
        let scope = home.join("scope");
        fs::create_dir(&scope).unwrap();
        Self { home, scope }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_glomeris"));
        command.env("HOME", &self.home).current_dir(&self.home);
        command
    }

    fn raw(&self, args: &[&str], input: Option<&Value>) -> Output {
        let mut command = self.command();
        command.args(["resource"]).args(args);
        if let Some(value) = input {
            command.stdin(Stdio::piped());
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(value.to_string().as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        } else {
            command.output().unwrap()
        }
    }

    fn spawn_request(&self) -> std::process::Child {
        let mut command = self.command();
        command
            .args(["resource", "native", "request", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().unwrap()
    }

    fn execute_node_modules(&self, project_root: &Path) -> Output {
        let node_modules = project_root.join("node_modules");
        self.command()
            .args([
                "execute",
                "--action-id",
                "node.clean.node_modules",
                "--resource-id",
            ])
            .arg(format!("node_modules:{}", node_modules.display()))
            .arg("--project-root")
            .arg(project_root)
            .arg("--json")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn request(&self, request: &Value) -> (Output, Value) {
        let output = self.raw(&["native", "request", "--json"], Some(request));
        let value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        (output, value)
    }

    fn accepted(&self, request: Value) -> Value {
        let (output, reply) = self.request(&request);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(reply["status"], "acknowledged");
        reply
    }

    fn register(&self) -> Value {
        self.accepted(
            json!({"op":"register", "request_id":id(), "kind":"cargo_target", "path":self.scope}),
        )
    }

    fn acquire(&self, registered: &Value, request_id: String) -> Value {
        self.accepted(json!({"op":"acquire", "request_id":request_id,
            "id":registered["id"], "generation":registered["generation"],
            "revision":registered["revision"], "host":"codex", "session":"fixture-session",
            "owner_pid":std::process::id()}))
    }

    fn acquire_as_cli_owner(&self, registered: &Value) -> Value {
        let mut command = self.command();
        command
            .args(["resource", "native", "request", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let request = json!({"op":"acquire", "request_id":id(), "id":registered["id"],
            "generation":registered["generation"], "revision":registered["revision"],
            "host":"codex", "session":"separate-owner", "owner_pid":child.id()});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn update(
        &self,
        base: &Value,
        capability: &Value,
        request_id: String,
        update: Value,
    ) -> (Output, Value) {
        self.request(&json!({"op":"update", "request_id":request_id,
            "id":base["id"], "generation":base["generation"], "revision":base["revision"],
            "claim":capability["claim"], "epoch":capability["epoch"], "token":capability["token"],
            "update":update}))
    }

    fn inspect(&self) -> Value {
        let output = self.raw(&["native", "inspect", "--json"], None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.store_root()
            .join("resources/records/native")
            .join(format!("{id}.json"))
    }

    fn store_root(&self) -> PathBuf {
        self.home.join("Library/Application Support/Glomeris")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}

fn id() -> String {
    format!(
        "{:08x}{:024x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
    )
}

fn fixture_git(home: &Path, args: &[&str]) -> Output {
    fixture_git_hostile(home, args, &[])
}

fn fixture_git_hostile(home: &Path, args: &[&str], hostile: &[(&str, &Path)]) -> Output {
    let mut command = Command::new("git");
    for (key, value) in hostile {
        command.env(key, value);
    }
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    for (key, _) in hostile {
        command.env_remove(key);
    }
    command
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn capability(reply: &Value) -> Value {
    reply["capability"].clone()
}

fn actor_cli(home: &Path, binary: &str, request: &Value) -> Value {
    let mut command = Command::new(binary);
    command
        .args(["resource", "native", "request", "--json"])
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn actor_cli_raw(home: &Path, binary: &str, request: &Value) -> Output {
    let mut command = Command::new(binary);
    command
        .args(["resource", "native", "request", "--json"])
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn actor_inspect(home: &Path, binary: &str) -> Value {
    let output = Command::new(binary)
        .args(["resource", "native", "inspect", "--json"])
        .env("HOME", home)
        .current_dir(home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn emit_actor_reply(reply: &Value) {
    println!("NATIVE_ACTOR:{}", reply);
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "subprocess actor; exercised by parent integration tests"]
fn native_parent_actor() {
    if std::env::var_os("GLOMERIS_NATIVE_PARENT_ACTOR").is_none() {
        return;
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let setup: Value = serde_json::from_str(&line).unwrap();
    let home = PathBuf::from(setup["home"].as_str().unwrap());
    let binary = setup["binary"].as_str().unwrap();
    let registered = &setup["registered"];

    let acquired = actor_cli(
        &home,
        binary,
        &json!({"op":"acquire", "request_id":id(), "id":registered["id"],
            "generation":registered["generation"], "revision":registered["revision"],
            "host":"codex", "session":"separate-parent", "owner_pid":std::process::id()}),
    );
    emit_actor_reply(&acquired);
    let parent_capability = capability(&acquired);
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let prepared = actor_cli(
        &home,
        binary,
        &json!({"op":"update", "request_id":id(), "id":base["id"],
            "generation":base["generation"], "revision":base["revision"],
            "claim":parent_capability["claim"], "epoch":parent_capability["epoch"],
            "token":parent_capability["token"], "update":{"kind":"prepare_child", "launch_id":setup["launch_id"]}}),
    );
    emit_actor_reply(&prepared);
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":prepared["revision"]});
    let confirmed = actor_cli(
        &home,
        binary,
        &json!({"op":"update", "request_id":id(), "id":base["id"],
            "generation":base["generation"], "revision":base["revision"],
            "claim":parent_capability["claim"], "epoch":parent_capability["epoch"],
            "token":parent_capability["token"], "update":{"kind":"confirm_child",
                "launch_id":setup["launch_id"], "pid":setup["child_pid"]}}),
    );
    emit_actor_reply(&confirmed);
    let release_revision = if setup["hold_after_confirm"] == true {
        let mut command = String::new();
        if std::io::stdin().read_line(&mut command).unwrap() == 0 {
            return;
        }
        serde_json::from_str::<Value>(&command).unwrap()["revision"].clone()
    } else {
        confirmed["revision"].clone()
    };
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":release_revision});
    let released = actor_cli(
        &home,
        binary,
        &json!({"op":"update", "request_id":id(), "id":base["id"],
            "generation":base["generation"], "revision":base["revision"],
            "claim":parent_capability["claim"], "epoch":parent_capability["epoch"],
            "token":parent_capability["token"], "update":{"kind":"release"}}),
    );
    emit_actor_reply(&released);
}

#[test]
#[ignore = "subprocess actor; exercised by parent integration tests"]
fn native_idle_child_actor() {
    if std::env::var_os("GLOMERIS_NATIVE_IDLE_CHILD").is_none() {
        return;
    }
    let mut exit_signal = String::new();
    std::io::stdin().read_line(&mut exit_signal).unwrap();
}

#[test]
#[ignore = "subprocess actor; exercised by parent integration tests"]
fn native_child_resume_actor() {
    if std::env::var_os("GLOMERIS_NATIVE_CHILD_RESUME_ACTOR").is_none() {
        return;
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let setup: Value = serde_json::from_str(&line).unwrap();
    let home = PathBuf::from(setup["home"].as_str().unwrap());
    let binary = setup["binary"].as_str().unwrap();
    let claim = setup["claim"].as_str().unwrap();
    let launch_id = setup["launch_id"].as_str().unwrap();
    let old_token = setup["child_token"].as_str().unwrap();
    let old_epoch = setup["child_epoch"].as_u64().unwrap();
    let registered = &setup["registered"];
    let revision = setup["revision"].as_u64().unwrap();

    let resume = json!({"op":"child_resume", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":revision, "claim":claim,
        "launch_id":launch_id, "token":old_token, "expected_child_epoch":old_epoch,
        "owner_pid":std::process::id()});
    let resumed = actor_cli(&home, binary, &resume);
    let new_capability = &resumed["child_capability"];
    let replay = actor_cli(&home, binary, &resume);
    assert_eq!(replay, resumed);

    let current_revision = resumed["revision"].as_u64().unwrap();
    let stale_release = json!({"op":"child_release", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":current_revision, "claim":claim,
        "launch_id":launch_id, "token":old_token, "expected_child_epoch":old_epoch});
    let stale_release_output = actor_cli_raw(&home, binary, &stale_release);
    assert!(!stale_release_output.status.success());
    let stale_resume = json!({"op":"child_resume", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":current_revision, "claim":claim,
        "launch_id":launch_id, "token":old_token, "expected_child_epoch":old_epoch,
        "owner_pid":std::process::id()});
    let stale_resume_output = actor_cli_raw(&home, binary, &stale_resume);
    assert!(!stale_resume_output.status.success());
    let closing_view = actor_inspect(&home, binary);
    assert_eq!(closing_view[0]["claims"][0]["state"], "closing");

    let child_release = json!({"op":"child_release", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":current_revision, "claim":claim,
        "launch_id":launch_id, "token":new_capability["child_token"],
        "expected_child_epoch":new_capability["child_epoch"]});
    let released = actor_cli(&home, binary, &child_release);
    let terminal_resume = json!({"op":"child_resume", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":released["revision"], "claim":claim,
        "launch_id":launch_id, "token":new_capability["child_token"],
        "expected_child_epoch":new_capability["child_epoch"], "owner_pid":std::process::id()});
    let terminal_output = actor_cli_raw(&home, binary, &terminal_resume);
    assert!(!terminal_output.status.success());
    println!(
        "NATIVE_RESUME_ACTOR:{}",
        json!({"resume":resumed,"release":released,"closing_view":closing_view})
    );
}

#[test]
fn register_acquire_binds_the_current_cli_caller_and_inspection_hides_capability() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let capability = capability(&acquired);
    assert_eq!(capability["epoch"], 1);
    assert!(capability["token"].as_str().unwrap().len() >= 32);
    let view = &f.inspect()[0];
    assert_eq!(view["claims"][0]["owner"]["pid"], std::process::id());
    assert!(!view["eligible"].as_bool().unwrap());
    assert_eq!(view["coverage"], "incomplete");
    assert!(!view["cleanup_authority"].as_bool().unwrap());
    assert!(view
        .to_string()
        .find(capability["token"].as_str().unwrap())
        .is_none());
}

#[test]
fn request_id_replays_identically_and_refuses_a_different_request() {
    let f = Fixture::new();
    let request_id = id();
    let request =
        json!({"op":"register", "request_id":request_id, "kind":"cargo_target", "path":f.scope});
    let first = f.accepted(request.clone());
    let replay = f.accepted(request.clone());
    assert_eq!(first, replay);
    let conflict =
        json!({"op":"register", "request_id":request_id, "kind":"cargo_target", "path":f.home});
    let (output, _) = f.request(&conflict);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("request ID reused"));
}

fn assert_store_overlap_registration_refused(f: &Fixture, path: &Path, sentinel: &Path) {
    let original = fs::read(sentinel).unwrap();
    let (output, _) = f.request(&json!({
        "op":"register", "request_id":id(), "kind":"cargo_target", "path":path
    }));
    assert!(
        !output.status.success(),
        "native registration must refuse store overlap"
    );
    assert_eq!(fs::read(sentinel).unwrap(), original);
    assert!(
        f.inspect().as_array().unwrap().is_empty(),
        "refusal must not create a native record"
    );
}

#[test]
fn registration_refuses_the_store_root_without_mutating_its_contents() {
    let f = Fixture::new();
    let store_root = f.store_root();
    fs::create_dir_all(&store_root).unwrap();
    let sentinel = store_root.join("registration-sentinel");
    fs::write(&sentinel, b"private store content\n").unwrap();
    assert_store_overlap_registration_refused(&f, &store_root, &sentinel);
}

#[test]
fn registration_refuses_a_store_ancestor_without_mutating_its_contents() {
    let f = Fixture::new();
    let sentinel = f.home.join("ancestor-sentinel");
    fs::write(&sentinel, b"private home content\n").unwrap();
    assert_store_overlap_registration_refused(&f, &f.home, &sentinel);
}

#[test]
fn aliases_dedupe_but_distinct_owners_keep_independent_claims() {
    let f = Fixture::new();
    let first = f.register();
    let alias = f.accepted(
        json!({"op":"register", "request_id":id(), "kind":"cargo_target",
        "path":f.scope.join("." )}),
    );
    assert_eq!(first["id"], alias["id"]);
    assert_eq!(first["generation"], alias["generation"]);
    let a = f.acquire_as_cli_owner(&alias);
    let mut after_first = alias.clone();
    after_first["revision"] = a["revision"].clone();
    let b = f.acquire_as_cli_owner(&after_first);
    assert_ne!(a["capability"]["claim"], b["capability"]["claim"]);
    assert_eq!(f.inspect()[0]["claims"].as_array().unwrap().len(), 2);
    assert_ne!(
        f.inspect()[0]["claims"][0]["owner"]["pid"],
        f.inspect()[0]["claims"][1]["owner"]["pid"]
    );
}

#[test]
fn expired_waiting_claim_is_preserved_and_renewal_does_not_count_as_use() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let cap = capability(&acquired);
    let waiting_base = json!({"id":registered["id"], "generation":registered["generation"],
        "revision":acquired["revision"]});
    let (out, waiting) = f.update(&waiting_base, &cap, id(), json!({"kind":"wait"}));
    assert!(out.status.success());
    let before = f.inspect()[0]["last_meaningful_use"].as_u64().unwrap();
    let record_path = f.record_path(registered["id"].as_str().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["claims"][0]["lease_until"] = json!(1);
    record["last_meaningful_use"] = json!(before.saturating_sub(90_000));
    fs::write(&record_path, record.to_string()).unwrap();
    let renew_base = json!({"id":registered["id"], "generation":registered["generation"], "revision":waiting["revision"]});
    let (out, _) = f.update(&renew_base, &cap, id(), json!({"kind":"renew"}));
    assert!(out.status.success());
    let inspected = &f.inspect()[0];
    assert_eq!(inspected["claims"][0]["state"], "waiting");
    assert!(inspected["claims"][0]["lease_until"].as_u64().unwrap() > 1);
    assert_eq!(
        inspected["last_meaningful_use"],
        before.saturating_sub(90_000)
    );
    assert!(!inspected["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn resume_rotates_token_and_fences_the_previous_capability() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let old = capability(&acquired);
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, resumed) = f.update(
        &base,
        &old,
        id(),
        json!({"kind":"resume", "owner_pid":std::process::id()}),
    );
    assert!(out.status.success());
    let new = capability(&resumed);
    assert_eq!(new["epoch"], 2);
    assert_ne!(new["token"], old["token"]);
    let stale_base = json!({"id":registered["id"], "generation":registered["generation"], "revision":resumed["revision"]});
    let (out, _) = f.update(&stale_base, &old, id(), json!({"kind":"renew"}));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("capability stale"));
}

#[test]
fn stale_generation_revision_and_replaced_root_are_refused() {
    let f = Fixture::new();
    let registered = f.register();
    let wrong_generation = json!({"op":"acquire", "request_id":id(), "id":registered["id"],
        "generation":id(), "revision":registered["revision"], "host":"codex", "session":"s", "owner_pid":std::process::id()});
    assert!(!f.request(&wrong_generation).0.status.success());
    let wrong_revision = json!({"op":"acquire", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":99, "host":"codex", "session":"s", "owner_pid":std::process::id()});
    assert!(!f.request(&wrong_revision).0.status.success());
    fs::remove_dir(&f.scope).unwrap();
    fs::create_dir(&f.scope).unwrap();
    let changed = json!({"op":"acquire", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":registered["revision"], "host":"codex", "session":"s", "owner_pid":std::process::id()});
    let (out, _) = f.request(&changed);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("scope identity changed"));
}

#[test]
fn child_intent_must_precede_confirmation_and_release_with_pending_child_closes_claim() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let cap = capability(&acquired);
    let launch_id = id();
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, _) = f.update(
        &base,
        &cap,
        id(),
        json!({"kind":"confirm_child", "launch_id":launch_id, "pid":std::process::id()}),
    );
    assert!(!out.status.success());
    let (out, prepared) = f.update(
        &base,
        &cap,
        id(),
        json!({"kind":"prepare_child", "launch_id":launch_id}),
    );
    assert!(out.status.success());
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":prepared["revision"]});
    let (out, released) = f.update(&next, &cap, id(), json!({"kind":"release"}));
    assert!(out.status.success());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");
    assert_eq!(f.inspect()[0]["claims"][0]["pending_children"], 1);
    assert!(!released["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn parent_closing_requires_explicit_child_capability_release_before_released() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let parent_capability = capability(&acquired);
    let launch_id = id();
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, prepared) = f.update(
        &base,
        &parent_capability,
        id(),
        json!({"kind":"prepare_child", "launch_id":launch_id}),
    );
    assert!(out.status.success());
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":prepared["revision"]});
    let (out, confirmed) = f.update(
        &next,
        &parent_capability,
        id(),
        json!({"kind":"confirm_child", "launch_id":launch_id, "pid":std::process::id()}),
    );
    assert!(out.status.success());
    let child_token = confirmed["child_capability"]["child_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":confirmed["revision"]});
    let (out, closing) = f.update(&next, &parent_capability, id(), json!({"kind":"release"}));
    assert!(out.status.success());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");

    let record_path = f.record_path(registered["id"].as_str().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["last_meaningful_use"] = json!(1);
    fs::write(&record_path, record.to_string()).unwrap();
    let repeat_base = json!({"id":registered["id"], "generation":registered["generation"],
        "revision":closing["revision"]});
    let (out, repeated_release) = f.update(
        &repeat_base,
        &parent_capability,
        id(),
        json!({"kind":"release"}),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after_repeat = &f.inspect()[0];
    assert_eq!(after_repeat["claims"][0]["state"], "closing");
    assert_eq!(after_repeat["last_meaningful_use"], 1);

    let child_epoch = confirmed["child_capability"]["child_epoch"].clone();
    let child_release = |request_id: String, token: Value| {
        json!({"op":"child_release",
        "request_id":request_id, "id":registered["id"], "generation":registered["generation"],
        "revision":repeated_release["revision"], "claim":parent_capability["claim"],
        "launch_id":launch_id, "token":token, "expected_child_epoch":child_epoch})
    };
    let (out, _) = f.request(&child_release(id(), json!("wrong")));
    assert!(!out.status.success());
    let child_request = child_release(id(), json!(child_token));
    let (out, child_released) = f.request(&child_request);
    assert!(out.status.success());
    assert!(!child_released["cleanup_authority"].as_bool().unwrap());
    let (replayed_output, replayed) = f.request(&child_request);
    assert!(replayed_output.status.success());
    assert_eq!(replayed, child_released);
    let view = &f.inspect()[0];
    assert_eq!(view["claims"][0]["state"], "released");
    assert!(view["claims"][0]["children"][0]["released"]
        .as_bool()
        .unwrap());
    assert_eq!(view["coverage"], "incomplete");
    assert!(!view["cleanup_authority"].as_bool().unwrap());
    assert!(view.to_string().find(&child_token).is_none());
}

#[test]
fn distinct_child_releases_after_parent_exit_without_parent_credentials_and_replays() {
    let f = Fixture::new();
    let registered = f.register();
    let launch_id = id();
    let mut actor = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_parent_actor", "--nocapture", "--ignored"])
        .env("GLOMERIS_NATIVE_PARENT_ACTOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let parent_pid = actor.id();
    assert_ne!(parent_pid, std::process::id());
    actor
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"home":f.home, "binary":env!("CARGO_BIN_EXE_glomeris"),
                "registered":registered, "launch_id":launch_id, "child_pid":std::process::id()})
            .to_string()
            .as_bytes(),
        )
        .unwrap();

    let mut output = BufReader::new(actor.stdout.take().unwrap());
    let mut replies = Vec::new();
    let mut line = String::new();
    while replies.len() < 4 && output.read_line(&mut line).unwrap() != 0 {
        if let Some(payload) = line.trim().strip_prefix("NATIVE_ACTOR:") {
            replies.push(serde_json::from_str::<Value>(payload).unwrap());
        }
        line.clear();
    }
    let status = actor.wait().unwrap();
    assert!(status.success());
    assert_eq!(replies.len(), 4);
    assert!(replies[0]["capability"]["claim"].is_string());
    assert_eq!(replies[2]["child_capability"]["child_launch_id"], launch_id);
    assert!(!replies[3]["cleanup_authority"].as_bool().unwrap());
    let parent_capability = capability(&replies[0]);
    let child_capability = &replies[2]["child_capability"];
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");

    // The child token cannot resume the parent claim; child release has no parent token or epoch.
    let resume = json!({"op":"update", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":replies[3]["revision"],
        "claim":parent_capability["claim"], "epoch":parent_capability["epoch"],
        "token":child_capability["child_token"],
        "update":{"kind":"resume", "owner_pid":std::process::id()}});
    assert!(!f.request(&resume).0.status.success());

    let child_release = json!({"op":"child_release", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":replies[3]["revision"],
        "claim":parent_capability["claim"], "launch_id":launch_id,
        "token":child_capability["child_token"], "expected_child_epoch":child_capability["child_epoch"]});
    let (first_output, first_reply) = f.request(&child_release);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    // Treat the first acknowledgement as lost and retry the identical request.
    let (retry_output, retry_reply) = f.request(&child_release);
    assert!(retry_output.status.success());
    assert_eq!(retry_reply, first_reply);

    let view = &f.inspect()[0];
    assert_eq!(view["claims"][0]["state"], "released");
    assert!(view["claims"][0]["children"][0]["released"]
        .as_bool()
        .unwrap());
    assert_eq!(view["coverage"], "incomplete");
    assert!(!view["cleanup_authority"].as_bool().unwrap());
    assert!(view
        .to_string()
        .find(child_capability["child_token"].as_str().unwrap())
        .is_none());
}

#[test]
fn exited_child_hands_off_only_with_child_token_and_new_owner_releases_parent() {
    let f = Fixture::new();
    let registered = f.register();
    let launch_id = id();
    let mut old_child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_idle_child_actor",
            "--nocapture",
            "--ignored",
        ])
        .env("GLOMERIS_NATIVE_IDLE_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let old_child_pid = old_child.id();

    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_parent_actor", "--nocapture", "--ignored"])
        .env("GLOMERIS_NATIVE_PARENT_ACTOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        parent.stdin.as_mut().unwrap(),
        "{}",
        json!({"home":f.home, "binary":env!("CARGO_BIN_EXE_glomeris"),
            "registered":registered, "launch_id":launch_id, "child_pid":old_child_pid})
    )
    .unwrap();
    let mut parent_output = BufReader::new(parent.stdout.take().unwrap());
    let mut parent_replies = Vec::new();
    let mut line = String::new();
    while parent_replies.len() < 4 && parent_output.read_line(&mut line).unwrap() != 0 {
        if let Some(payload) = line.trim().strip_prefix("NATIVE_ACTOR:") {
            parent_replies.push(serde_json::from_str::<Value>(payload).unwrap());
        }
        line.clear();
    }
    assert!(parent.wait().unwrap().success());
    assert_eq!(parent_replies.len(), 4);
    let parent_capability = capability(&parent_replies[0]);
    let child_capability = parent_replies[2]["child_capability"].clone();
    assert_eq!(child_capability["child_epoch"], 1);
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");

    writeln!(old_child.stdin.as_mut().unwrap(), "exit").unwrap();
    assert!(old_child.wait().unwrap().success());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");

    let mut replacement = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_child_resume_actor",
            "--nocapture",
            "--ignored",
        ])
        .env("GLOMERIS_NATIVE_CHILD_RESUME_ACTOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        replacement.stdin.as_mut().unwrap(),
        "{}",
        json!({"home":f.home, "binary":env!("CARGO_BIN_EXE_glomeris"), "registered":registered,
            "claim":parent_capability["claim"], "launch_id":launch_id,
            "child_token":child_capability["child_token"],
            "child_epoch":child_capability["child_epoch"], "revision":parent_replies[3]["revision"]})
    )
    .unwrap();
    let output = replacement.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("NATIVE_RESUME_ACTOR:")),
        "{stdout}"
    );
    let view = &f.inspect()[0];
    assert_eq!(view["claims"][0]["state"], "released");
    assert!(view["claims"][0]["children"][0]["released"]
        .as_bool()
        .unwrap());
    assert_eq!(view["coverage"], "incomplete");
    assert!(!view["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn already_released_child_fresh_requests_do_not_block_distinct_parent_release() {
    let f = Fixture::new();
    let registered = f.register();
    let launch_id = id();
    let mut actor = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_parent_actor", "--nocapture", "--ignored"])
        .env("GLOMERIS_NATIVE_PARENT_ACTOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_ne!(actor.id(), std::process::id());
    let setup = json!({"home":f.home, "binary":env!("CARGO_BIN_EXE_glomeris"),
        "registered":registered, "launch_id":launch_id, "child_pid":std::process::id(),
        "hold_after_confirm":true});
    let input = actor.stdin.as_mut().unwrap();
    writeln!(input, "{setup}").unwrap();
    input.flush().unwrap();

    let mut output = BufReader::new(actor.stdout.take().unwrap());
    let mut replies = Vec::new();
    let mut line = String::new();
    while replies.len() < 3 && output.read_line(&mut line).unwrap() != 0 {
        if let Some(payload) = line.trim().strip_prefix("NATIVE_ACTOR:") {
            replies.push(serde_json::from_str::<Value>(payload).unwrap());
        }
        line.clear();
    }
    assert_eq!(replies.len(), 3);
    let parent_capability = capability(&replies[0]);
    let child_capability = &replies[2]["child_capability"];
    assert_eq!(child_capability["child_epoch"], 1);
    let mut revision = replies[2]["revision"].as_u64().unwrap();
    let child_release = |request_id: String, revision: u64| {
        json!({"op":"child_release",
        "request_id":request_id, "id":registered["id"], "generation":registered["generation"],
        "revision":revision, "claim":parent_capability["claim"], "launch_id":launch_id,
        "token":child_capability["child_token"], "expected_child_epoch":child_capability["child_epoch"]})
    };

    let (out, first) = f.request(&child_release(id(), revision));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(first["status"], "acknowledged");
    revision = first["revision"].as_u64().unwrap();
    let before_retries = f.inspect()[0].clone();
    let meaningful_use = before_retries["last_meaningful_use"].clone();
    for _ in 0..260 {
        let (out, already_released) = f.request(&child_release(id(), revision));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(already_released["status"], "already_released");
        assert_eq!(already_released["revision"], revision);
    }
    let after_retries = &f.inspect()[0];
    assert_eq!(after_retries["revision"], revision);
    assert_eq!(after_retries["last_meaningful_use"], meaningful_use);

    writeln!(actor.stdin.as_mut().unwrap(), "{{\"revision\":{revision}}}").unwrap();
    actor.stdin.as_mut().unwrap().flush().unwrap();
    while replies.len() < 4 && output.read_line(&mut line).unwrap() != 0 {
        if let Some(payload) = line.trim().strip_prefix("NATIVE_ACTOR:") {
            replies.push(serde_json::from_str::<Value>(payload).unwrap());
        }
        line.clear();
    }
    assert_eq!(replies.len(), 4);
    let status = actor.wait().unwrap();
    assert!(status.success());
    assert!(!replies[3]["cleanup_authority"].as_bool().unwrap());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "released");
}

#[test]
fn confirmation_resolves_a_prepared_child_at_the_admission_boundary() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let capability = capability(&acquired);
    let launch_id = id();
    let mut revision = acquired["revision"].as_u64().unwrap();
    let base = |revision| json!({"id":registered["id"], "generation":registered["generation"], "revision":revision});
    let (out, prepared) = f.update(
        &base(revision),
        &capability,
        id(),
        json!({"kind":"prepare_child", "launch_id":launch_id}),
    );
    assert!(out.status.success());
    revision = prepared["revision"].as_u64().unwrap();
    for _ in 0..61 {
        let (out, reply) = f.update(&base(revision), &capability, id(), json!({"kind":"wait"}));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        revision = reply["revision"].as_u64().unwrap();
    }
    let record: Value = serde_json::from_slice(
        &fs::read(f.record_path(registered["id"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(record["receipts"].as_array().unwrap().len(), 64);

    let (out, confirmed) = f.update(
        &base(revision),
        &capability,
        id(),
        json!({"kind":"confirm_child", "launch_id":launch_id, "pid":std::process::id()}),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let base = base(confirmed["revision"].as_u64().unwrap());
    let (out, closing) = f.update(&base, &capability, id(), json!({"kind":"release"}));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "closing");
    let child_release = json!({"op":"child_release", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":closing["revision"],
        "claim":capability["claim"], "launch_id":launch_id,
        "token":confirmed["child_capability"]["child_token"],
        "expected_child_epoch":confirmed["child_capability"]["child_epoch"]});
    let (out, released_child) = f.request(&child_release);
    assert!(out.status.success());
    assert_eq!(released_child["status"], "acknowledged");
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "released");
    assert!(!released_child["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn acquired_request_replay_is_rejected_after_resume_and_release() {
    let f = Fixture::new();
    let registered = f.register();
    let acquisition = json!({"op":"acquire", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":registered["revision"],
        "host":"codex", "session":"replay-fence", "owner_pid":std::process::id()});
    let acquired = f.accepted(acquisition.clone());
    let old_capability = capability(&acquired);
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, resumed) = f.update(
        &base,
        &old_capability,
        id(),
        json!({"kind":"resume", "owner_pid":std::process::id()}),
    );
    assert!(out.status.success());
    let (out, _) = f.request(&acquisition);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("capability was superseded"));

    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":resumed["revision"]});
    let (out, released) = f.update(
        &base,
        &capability(&resumed),
        id(),
        json!({"kind":"release"}),
    );
    assert!(out.status.success());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "released");
    let (out, _) = f.request(&acquisition);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("capability was superseded"));
    assert!(!released["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn update_rejects_a_fixture_identity_with_the_right_pid_but_wrong_start() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let capability = capability(&acquired);
    let record_path = f.record_path(registered["id"].as_str().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["claims"][0]["owner"]["pid"], std::process::id());
    record["claims"][0]["owner"]["start"] = json!("fictional process start");
    fs::write(&record_path, record.to_string()).unwrap();

    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, _) = f.update(&base, &capability, id(), json!({"kind":"renew"}));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("current process-bound owner"));
}

#[test]
fn closure_transitions_remain_admissible_after_many_renewals() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let capability = capability(&acquired);
    let mut revision = acquired["revision"].as_u64().unwrap();
    for _ in 0..260 {
        let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":revision});
        let (out, renewed) = f.update(&base, &capability, id(), json!({"kind":"renew"}));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        revision = renewed["revision"].as_u64().unwrap();
    }
    let base =
        json!({"id":registered["id"], "generation":registered["generation"], "revision":revision});
    let (out, released) = f.update(&base, &capability, id(), json!({"kind":"release"}));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "released");
    assert!(!released["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn launch_id_cannot_be_reused_after_child_history_and_unlaunched_intent_can_be_cancelled() {
    let f = Fixture::new();
    let registered = f.register();
    let acquired = f.acquire(&registered, id());
    let cap = capability(&acquired);
    let launch_id = id();
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, prepared) = f.update(
        &base,
        &cap,
        id(),
        json!({"kind":"prepare_child", "launch_id":launch_id}),
    );
    assert!(out.status.success());
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":prepared["revision"]});
    let (out, confirmed) = f.update(
        &next,
        &cap,
        id(),
        json!({"kind":"confirm_child", "launch_id":launch_id, "pid":std::process::id()}),
    );
    assert!(out.status.success());
    let child_release = json!({"op":"child_release", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":confirmed["revision"], "claim":cap["claim"],
        "launch_id":launch_id, "token":confirmed["child_capability"]["child_token"],
        "expected_child_epoch":confirmed["child_capability"]["child_epoch"]});
    let (out, released_child) = f.request(&child_release);
    assert!(out.status.success());
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":released_child["revision"]});
    let (out, _) = f.update(
        &next,
        &cap,
        id(),
        json!({"kind":"prepare_child", "launch_id":launch_id}),
    );
    assert!(!out.status.success());

    let cancel_id = id();
    let (out, pending) = f.update(
        &next,
        &cap,
        cancel_id.clone(),
        json!({"kind":"prepare_child", "launch_id":cancel_id}),
    );
    assert!(out.status.success());
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":pending["revision"]});
    // Cancellation records the current owner's attestation; it does not observe whether a launch occurred.
    let (out, cancelled) = f.update(
        &next,
        &cap,
        id(),
        json!({"kind":"cancel_unlaunched", "launch_id":cancel_id}),
    );
    assert!(out.status.success());
    assert_eq!(f.inspect()[0]["claims"][0]["pending_children"], 0);
    let next = json!({"id":registered["id"], "generation":registered["generation"], "revision":cancelled["revision"]});
    let (out, _) = f.update(&next, &cap, id(), json!({"kind":"release"}));
    assert!(out.status.success());
    assert_eq!(f.inspect()[0]["claims"][0]["state"], "released");
}

#[test]
fn native_update_ignores_mixed_managed_history_and_preserves_no_authority() {
    let f = Fixture::new();
    let registered = f.register();
    let records = f
        .home
        .join("Library/Application Support/Glomeris/resources/records");
    fs::create_dir_all(&records).unwrap();
    for index in 0..257 {
        let managed_id = format!("{index:032x}");
        fs::write(
            records.join(format!("{managed_id}.json")),
            b"corrupt unrelated managed history",
        )
        .unwrap();
    }
    let acquired = f.acquire(&registered, id());
    let cap = capability(&acquired);
    let base = json!({"id":registered["id"], "generation":registered["generation"], "revision":acquired["revision"]});
    let (out, renewed) = f.update(&base, &cap, id(), json!({"kind":"renew"}));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!renewed["cleanup_authority"].as_bool().unwrap());
    let view = &f.inspect()[0];
    assert_eq!(view["coverage"], "incomplete");
    assert!(!view["cleanup_authority"].as_bool().unwrap());
}

#[test]
fn malformed_unknown_fields_and_oversized_input_fail_with_bounded_errors() {
    let f = Fixture::new();
    let malformed = f.raw(
        &["native", "request", "--json"],
        Some(&json!({"op":"register", "unknown":true})),
    );
    assert!(!malformed.status.success());
    let oversized = " ".repeat(65_537);
    let mut command = f.command();
    command
        .args(["resource", "native", "request", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(oversized.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exceeds 64KiB"));
}

#[test]
fn concurrent_requests_at_one_revision_allow_only_one_acknowledgement() {
    let f = Fixture::new();
    let registered = f.register();
    let make_request = || {
        json!({"op":"acquire", "request_id":id(), "id":registered["id"],
        "generation":registered["generation"], "revision":registered["revision"], "host":"codex",
        "session":"concurrent", "owner_pid":std::process::id()})
    };
    let requests = [make_request(), make_request()];
    let children = [f.spawn_request(), f.spawn_request()];
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = children
        .into_iter()
        .zip(requests)
        .map(|(mut child, request)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(request.to_string().as_bytes()).unwrap();
                drop(stdin);
                child.wait_with_output().unwrap()
            })
        })
        .collect();
    barrier.wait();
    let outputs: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(outputs.iter().filter(|o| o.status.success()).count(), 1);
    let failure = outputs.iter().find(|o| !o.status.success()).unwrap();
    let failure_text = String::from_utf8_lossy(&failure.stderr);
    assert!(
        failure_text.contains("busy")
            || failure_text.contains("stale")
            || failure_text.contains("revision"),
        "losing concurrent request should be busy or stale: {failure_text}"
    );
    let after = f.inspect();
    assert_eq!(after[0]["claims"].as_array().unwrap().len(), 1);
    assert_eq!(
        after[0]["revision"].as_u64(),
        registered["revision"].as_u64().map(|r| r + 1)
    );
}

fn autosafe_node_modules(home: &Path, name: &str) -> PathBuf {
    let project = home.join(name);
    let modules = project.join("node_modules");
    fs::create_dir_all(&modules).unwrap();
    fs::write(modules.join("package.json"), "{}").unwrap();
    project.canonicalize().unwrap()
}

#[test]
fn execute_cli_succeeds_for_private_autosafe_node_modules() {
    let f = Fixture::new();
    let project = autosafe_node_modules(&f.home, "autosafe-project");
    let output = f.execute_node_modules(&project);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["outcome"], "succeeded");
    assert!(!project.join("node_modules").exists());
}

#[test]
fn execute_cli_refuses_native_same_kind_overlap_and_preserves_files() {
    let f = Fixture::new();
    let project = autosafe_node_modules(&f.home, "overlap-project");
    let modules = project.join("node_modules");
    let registration = f.accepted(
        json!({"op":"register", "request_id":id(), "kind":"cargo_target", "path":project}),
    );
    assert!(!registration["cleanup_authority"].as_bool().unwrap());

    let output = f.execute_node_modules(&project);
    assert!(
        !output.status.success(),
        "overlapping native scope must refuse execution"
    );
    assert!(
        modules.join("package.json").exists(),
        "refusal must preserve fixture files"
    );
}

#[test]
fn missing_docker_socket_does_not_block_unrelated_positive_execution() {
    let f = Fixture::new();
    let socket = f.home.join("docker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    f.accepted(json!({"op":"register", "request_id":id(), "kind":"docker_builder", "path":socket, "builder":"fixture-builder"}));
    drop(listener);
    fs::remove_file(&socket).unwrap();

    let project = autosafe_node_modules(&f.home, "unrelated-project");
    let output = f.execute_node_modules(&project);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["outcome"], "succeeded");
    assert!(!project.join("node_modules").exists());
}

#[test]
fn owned_worktree_and_unserviced_docker_socket_can_be_registered_as_evidence() {
    let f = Fixture::new();
    let repo = f.home.join("repo");
    fs::create_dir(&repo).unwrap();
    let hooks = f.home.join("fixture-hooks");
    fs::create_dir(&hooks).unwrap();
    for args in [
        vec!["-C", repo.to_str().unwrap(), "init", "-q"],
        vec![
            "-C",
            repo.to_str().unwrap(),
            "config",
            "user.name",
            "Fixture",
        ],
        vec![
            "-C",
            repo.to_str().unwrap(),
            "config",
            "user.email",
            "fixture@example.invalid",
        ],
        vec![
            "-C",
            repo.to_str().unwrap(),
            "config",
            "core.hooksPath",
            hooks.to_str().unwrap(),
        ],
    ] {
        assert!(fixture_git(&f.home, &args).status.success());
    }
    fs::write(repo.join("tracked"), "fixture").unwrap();
    assert!(
        fixture_git(&f.home, &["-C", repo.to_str().unwrap(), "add", "tracked"])
            .status
            .success()
    );
    assert!(fixture_git(
        &f.home,
        &["-C", repo.to_str().unwrap(), "commit", "-qm", "fixture"]
    )
    .status
    .success());
    let worktree = f.home.join("worktree");
    assert!(fixture_git(
        &f.home,
        &[
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "fixture-worktree"
        ]
    )
    .status
    .success());
    let wt =
        f.accepted(json!({"op":"register", "request_id":id(), "kind":"worktree", "path":worktree}));
    assert!(!wt["cleanup_authority"].as_bool().unwrap());

    let socket = f.home.join("docker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let docker = f.accepted(json!({"op":"register", "request_id":id(), "kind":"docker_builder", "path":socket, "builder":"fixture-builder"}));
    assert!(!docker["cleanup_authority"].as_bool().unwrap());
    drop(listener);
}

#[test]
fn git_observation_ignores_host_git_environment_and_preserves_external_sentinels() {
    let f = Fixture::new();
    let external = f.home.join("external");
    fs::create_dir(&external).unwrap();
    assert!(
        fixture_git(&f.home, &["-C", external.to_str().unwrap(), "init", "-q"])
            .status
            .success()
    );
    let external_git = external.join(".git");
    let external_git_config = fs::read(external_git.join("config")).unwrap();
    let config_sentinel = f.home.join("host-global.gitconfig");
    fs::write(&config_sentinel, b"[user]\n\tname = sentinel\n").unwrap();
    let before_config = fs::read(&config_sentinel).unwrap();
    let fixture_repo = f.home.join("observation-repo");
    fs::create_dir(&fixture_repo).unwrap();
    let missing_system_config = f.home.join("missing-system-config");
    let hostile_git_env = [
        ("GIT_DIR", external_git.as_path()),
        ("GIT_WORK_TREE", external.as_path()),
        ("GIT_CONFIG_GLOBAL", config_sentinel.as_path()),
        ("GIT_CONFIG_SYSTEM", missing_system_config.as_path()),
    ];
    assert!(fixture_git_hostile(
        &f.home,
        &["-C", fixture_repo.to_str().unwrap(), "init", "-q"],
        &hostile_git_env
    )
    .status
    .success());
    let mut hostile = f.command();
    hostile
        .args(["resource", "native", "request", "--json"])
        .env("GIT_DIR", &external_git)
        .env("GIT_WORK_TREE", &external)
        .env("GIT_CONFIG_GLOBAL", &config_sentinel)
        .env("GIT_CONFIG_SYSTEM", f.home.join("missing-system-config"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", f.home.join("host-hooks"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = hostile.spawn().unwrap();
    let request =
        json!({"op":"register", "request_id":id(), "kind":"worktree", "path":fixture_repo});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["status"], "acknowledged");
    let view = f
        .inspect()
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == reply["id"])
        .unwrap()
        .clone();
    assert_eq!(
        view["scope"]["path"],
        fixture_repo.to_string_lossy().as_ref()
    );
    assert_eq!(
        view["scope"]["registration"],
        fixture_repo.join(".git").to_string_lossy().as_ref()
    );
    assert_eq!(fs::read(&config_sentinel).unwrap(), before_config);
    assert_eq!(
        fs::read(external_git.join("config")).unwrap(),
        external_git_config
    );
}

#[test]
fn replaced_git_metadata_fences_a_registered_worktree_owner() {
    let f = Fixture::new();
    let repo = f.home.join("identity-repo");
    fs::create_dir(&repo).unwrap();
    let repo_arg = repo.to_str().unwrap();
    assert!(fixture_git(&f.home, &["-C", repo_arg, "init", "-q"])
        .status
        .success());
    assert!(
        fixture_git(&f.home, &["-C", repo_arg, "config", "user.name", "Fixture"])
            .status
            .success()
    );
    assert!(fixture_git(
        &f.home,
        &[
            "-C",
            repo_arg,
            "config",
            "user.email",
            "fixture@example.invalid"
        ]
    )
    .status
    .success());
    let tracked = repo.join("tracked.txt");
    fs::write(&tracked, "source preserved\n").unwrap();
    assert!(
        fixture_git(&f.home, &["-C", repo_arg, "add", "tracked.txt"])
            .status
            .success()
    );
    assert!(
        fixture_git(&f.home, &["-C", repo_arg, "commit", "-qm", "fixture"])
            .status
            .success()
    );

    let registered =
        f.accepted(json!({"op":"register", "request_id":id(), "kind":"worktree", "path":repo}));
    let acquired = f.acquire(&registered, id());
    let cap = capability(&acquired);
    fs::rename(repo.join(".git"), repo.join(".git-preserved")).unwrap();
    assert!(fixture_git(&f.home, &["-C", repo_arg, "init", "-q"])
        .status
        .success());

    let base = json!({"id":registered["id"], "generation":registered["generation"],
        "revision":acquired["revision"]});
    let (output, _) = f.update(&base, &cap, id(), json!({"kind":"wait"}));
    assert!(
        !output.status.success(),
        "replaced Git metadata must fence stale ownership"
    );
    assert_eq!(fs::read(&tracked).unwrap(), b"source preserved\n");
    assert!(repo.join(".git-preserved").is_dir());
}
