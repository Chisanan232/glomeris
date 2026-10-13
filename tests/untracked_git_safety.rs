use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    home: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "glomeris-untracked-git-{name}-{}-{nonce}",
            std::process::id()
        ));
        let root = home.join("project");
        std::fs::create_dir_all(&root).expect("create isolated fixture");

        git(&root, &["init", "--quiet"]);
        std::fs::write(root.join(".gitignore"), "node_modules/\n")
            .expect("write fixture ignore rule");
        git(&root, &["config", "user.name", "Fixture"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(&root, &["add", ".gitignore"]);
        git(&root, &["commit", "--quiet", "-m", "fixture baseline"]);

        let home = home.canonicalize().expect("canonical fixture home");
        let root = home.join("project");
        let generated = root.join("node_modules");
        std::fs::create_dir_all(&generated).expect("create ignored generated cache");
        std::fs::write(generated.join("package.json"), "{}\n")
            .expect("write reconstructable cache fixture");

        Self { home, root }
    }

    fn resource_id(&self) -> String {
        format!("node_modules:{}", self.root.join("node_modules").display())
    }

    fn explain(&self) -> Output {
        glomeris()
            .arg("explain")
            .arg(self.resource_id())
            .arg("--project-root")
            .arg(&self.root)
            .arg("--json")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("no-global-config"))
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .expect("run explain against isolated fixture")
    }

    fn execute(&self) -> Output {
        glomeris()
            .arg("execute")
            .arg("--action-id")
            .arg("node.clean.node_modules")
            .arg("--resource-id")
            .arg(self.resource_id())
            .arg("--project-root")
            .arg(&self.root)
            .arg("--json")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("no-global-config"))
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .expect("run execute against isolated fixture")
    }

    fn assert_generated_cache_exists(&self) {
        assert!(
            self.root.join("node_modules/package.json").is_file(),
            "the ignored generated fixture must remain intact"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global-config"))
        .output()
        .expect("run local fixture git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn glomeris() -> Command {
    Command::new(env!("CARGO_BIN_EXE_glomeris"))
}

fn json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("expected JSON, got {error}: {stdout}"))
}

fn assert_ask_for_untracked(fixture: &Fixture) {
    let explained = fixture.explain();
    assert_eq!(
        explained.status.code(),
        Some(0),
        "explain must return its report: {}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let report = json(&explained);
    assert_eq!(report["policy_label"], "ASK", "report: {report}");
    assert_eq!(report["git"]["dirty"], false, "report: {report}");
    use glomeris::evidence::correlate::{GitCliProbe, GitProbe};
    use glomeris::evidence::probe::ProbeOutcome;
    assert!(matches!(
        GitCliProbe.state_of(&fixture.root, std::time::Duration::from_secs(2)),
        ProbeOutcome::Observed(Some(state)) if state.untracked && !state.dirty
    ));
    assert!(
        report["active_use_signals"]
            .as_array()
            .is_some_and(|signals| signals.iter().any(|signal| {
                signal
                    .as_str()
                    .is_some_and(|signal| signal.to_ascii_lowercase().contains("untracked"))
            })),
        "explain must report the untracked active-use signal: {report}"
    );

    let executed = fixture.execute();
    assert_eq!(
        executed.status.code(),
        Some(3),
        "ASK without explicit consent must refuse: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let refusal = json(&executed);
    assert_eq!(refusal["reason"], "ask_no_consent", "report: {refusal}");
    fixture.assert_generated_cache_exists();
}

#[test]
fn clean_repository_with_ignored_reconstructable_cache_is_auto_safe_and_executes() {
    let fixture = Fixture::new("clean");
    let explained = fixture.explain();
    assert_eq!(
        explained.status.code(),
        Some(0),
        "explain must return its report: {}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let report = json(&explained);
    assert_eq!(report["policy_label"], "AUTO_SAFE", "report: {report}");

    let executed = fixture.execute();
    assert_eq!(
        executed.status.code(),
        Some(0),
        "clean ignored cache should execute: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(json(&executed)["outcome"], "succeeded");
    assert!(!fixture.root.join("node_modules").exists());
}

#[test]
fn an_untracked_owner_file_makes_ignored_cache_ask_and_preserves_both() {
    let fixture = Fixture::new("owner-file");
    let owner_file = fixture.root.join("important-local-notes.txt");
    std::fs::write(&owner_file, "unique owner data\n").expect("write untracked owner file");

    assert_ask_for_untracked(&fixture);
    assert_eq!(
        std::fs::read_to_string(&owner_file).expect("owner data remains"),
        "unique owner data\n"
    );
}

#[test]
fn disabling_git_status_untracked_display_does_not_hide_owner_files() {
    let fixture = Fixture::new("status-config");
    git(
        &fixture.root,
        &["config", "status.showUntrackedFiles", "no"],
    );
    let owner_file = fixture.root.join("important-local-notes.txt");
    std::fs::write(&owner_file, "unique owner data\n").expect("write untracked owner file");

    assert_ask_for_untracked(&fixture);
    assert_eq!(
        std::fs::read_to_string(owner_file).unwrap(),
        "unique owner data\n"
    );
}

#[test]
fn an_untracked_directory_makes_ignored_cache_ask_and_preserves_both() {
    let fixture = Fixture::new("owner-directory");
    let owner_dir = fixture.root.join("local-output");
    std::fs::create_dir_all(&owner_dir).expect("create untracked owner directory");
    let owner_file = owner_dir.join("result.txt");
    std::fs::write(&owner_file, "unique owner data\n").expect("write untracked owner file");

    assert_ask_for_untracked(&fixture);
    assert_eq!(
        std::fs::read_to_string(owner_file).unwrap(),
        "unique owner data\n"
    );
}
