//! HORO-1484 regression proof: a detector whose probe FAILED is reported as
//! a failure, distinguishably from a detector whose tool is merely absent,
//! everywhere the CLI reports on discovery.
//!
//! The defect: `Failed` and `ToolAbsent` were matched into the same
//! do-nothing arm, so a probe that errored contributed nothing to any
//! report. `detect --json` then rendered a partial search exactly like a
//! complete one that found less, and the `--progress-json` stream reported
//! both states as the same `candidates_found: 0` event. The codebase's own
//! stated rule for `ToolAbsent` — "normal, expected state, never an error" —
//! had been extended to `Failed`, for which it is false: a probe that did
//! not answer means whatever it would have found is unknown.
//!
//! Every test here spawns the real compiled binary, and the *failure* is
//! produced by a real detector taking its real failure path: a `brew` shim
//! first on `PATH` that exits non-zero, which
//! `detectors::homebrew::HomebrewDetector` reports as
//! `DetectorStatus::Failed`. Nothing here can reach real Homebrew or Docker
//! state — both tools are shimmed for the child process, `$HOME` is an empty
//! temporary directory, the only `--project-root` is a temp fixture, and
//! `detect` is read-only in any case.
//!
//! The `free` and `emergency` halves of this ticket are asserted in-crate
//! (`executor::recovery_loop::run_tests` and `emergency::tests`) rather than
//! here, deliberately: both commands really delete things, and the in-crate
//! tests can substitute a fake detector registry via
//! `DetectorRegistry::from_detectors` instead of letting the real detectors
//! loose on the host. Running `glomeris free` or `glomeris emergency` from an
//! integration test to observe their output would mean executing real
//! reclamation against the machine running the suite.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn make_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
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

fn write_shim(path: &Path, script: &str) {
    fs::write(path, script).expect("write shim script");
    let mut perms = fs::metadata(path).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod shim");
}

/// A `PATH` directory holding a `brew` and a `docker` shim, plus the fixture
/// directories the detectors read.
///
/// `brew` behaves one of two ways depending on `brew_fails`:
///
/// * failing — exits 1, which `HomebrewDetector` reports as
///   `DetectorStatus::Failed("brew --cache exited with status ...")`. This is
///   the real detector's real failure path, not an injected status.
/// * succeeding — prints a cache directory that exists and holds one file,
///   which it reports as `Found`.
///
/// `docker` always exits 1, which `DockerDetector` treats as "daemon
/// unreachable" and reports as `ToolAbsent`. That is the whole point of the
/// fixture: one run contains a detector that failed *and* a detector whose
/// tool is absent, so a report that renders them identically fails these
/// tests.
struct Fixture {
    shim_dir: PathBuf,
    home: PathBuf,
    project_root: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(prefix: &str, brew_fails: bool) -> Self {
        let root = make_temp_dir(prefix);
        let shim_dir = root.join("bin");
        fs::create_dir_all(&shim_dir).expect("create shim dir");

        let brew_script = if brew_fails {
            "#!/bin/sh\necho 'brew: simulated failure' >&2\nexit 1\n".to_string()
        } else {
            let cache_dir = root.join("brew-cache");
            fs::create_dir_all(&cache_dir).expect("create fake brew cache");
            fs::write(cache_dir.join("some-bottle.tar.gz"), vec![0u8; 2048])
                .expect("write fake brew cache file");
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", cache_dir.display())
        };
        write_shim(&shim_dir.join("brew"), &brew_script);
        write_shim(&shim_dir.join("docker"), "#!/bin/sh\nexit 1\n");

        let home = root.join("home");
        fs::create_dir_all(&home).expect("create empty home");

        // A cargo `target/` dir so at least one detector reports real
        // candidates. The defect's shape is "a partly-failed discovery
        // rendering as complete", not "an empty list", so the interesting
        // case has candidates in it.
        let project_root = root.join("project");
        let target_dir = project_root.join("target/debug");
        fs::create_dir_all(&target_dir).expect("create cargo target dir");
        fs::write(target_dir.join("build_output.bin"), vec![0u8; 4096])
            .expect("write cargo fixture");

        Self {
            shim_dir,
            home,
            project_root,
            root,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut full: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        full.push("--project-root".to_string());
        full.push(self.project_root.to_string_lossy().into_owned());

        Command::new(glomeris_bin())
            .args(&full)
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.shim_dir.display()))
            .stdin(Stdio::null())
            .output()
            .expect("failed to spawn glomeris binary")
    }

    fn cleanup(self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

/// Finds one detector's entry in a `DetectReport`'s `detectors` array.
fn detector_entry<'a>(report: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    report
        .get("detectors")
        .unwrap_or_else(|| {
            panic!(
                "detect --json must carry a `detectors` array; got:\n{}",
                serde_json::to_string_pretty(report).unwrap_or_default()
            )
        })
        .as_array()
        .expect("`detectors` must be an array")
        .iter()
        .find(|entry| entry.get("detector").and_then(|v| v.as_str()) == Some(id))
        .unwrap_or_else(|| {
            panic!(
                "no entry for detector {id:?}; got:\n{}",
                serde_json::to_string_pretty(report).unwrap_or_default()
            )
        })
}

/// AC 2: `detect --json` distinguishes a failed detector from an absent
/// tool, carries the failure's reason, and does not present the run as a
/// complete account of what is reclaimable.
#[test]
fn detect_json_distinguishes_a_failed_detector_from_an_absent_tool() {
    let fixture = Fixture::new("horo1484-json-failed", true);
    let output = fixture.run(&["detect", "--json"]);
    assert!(
        output.status.success(),
        "glomeris detect --json should succeed even when a detector failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("detect --json must print one JSON document");

    // The failed detector.
    let brew = detector_entry(&report, "homebrew_cache");
    assert_eq!(
        brew.get("status").and_then(|v| v.as_str()),
        Some("failed"),
        "the shimmed `brew` exits non-zero, which HomebrewDetector reports as \
         Failed; got: {brew}"
    );
    let reason = brew
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("a failed detector must carry its reason; got: {brew}"));
    assert!(
        !reason.trim().is_empty(),
        "the reason must say something; got: {reason:?}"
    );

    // The absent tool, in the same document. This is the pairing the defect
    // destroyed: before the fix both were reported identically.
    let docker = detector_entry(&report, "docker_build_cache");
    assert_eq!(
        docker.get("status").and_then(|v| v.as_str()),
        Some("tool_absent"),
        "an unreachable docker daemon is a tool-absent answer, not a failure; got: {docker}"
    );
    assert!(
        docker.get("reason").is_none(),
        "`tool_absent` is not a failure and carries no failure reason; got: {docker}"
    );

    // The completeness claim.
    assert_eq!(
        report.get("discovery_complete").and_then(|v| v.as_bool()),
        Some(false),
        "a run in which a detector failed did not complete discovery; got:\n{stdout}"
    );

    // ...and it is *not* an empty run. A report that only qualified itself
    // when it had nothing to show would miss the actual defect.
    let candidates = report
        .get("candidates")
        .and_then(|v| v.as_array())
        .expect("a DetectReport always has a `candidates` array");
    assert!(
        !candidates.is_empty(),
        "the cargo fixture must produce at least one candidate, so this asserts \
         an incomplete discovery that nonetheless found things; got:\n{stdout}"
    );

    fixture.cleanup();
}

/// The anti-vacuity partner: with every detector answering, the same command
/// over the same fixture reports discovery as complete and flags nothing as
/// failed. Without this, `discovery_complete: false` could be hardwired.
#[test]
fn detect_json_reports_complete_discovery_when_every_detector_answered() {
    let fixture = Fixture::new("horo1484-json-complete", false);
    let output = fixture.run(&["detect", "--json"]);
    assert!(
        output.status.success(),
        "glomeris detect --json should succeed"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("detect --json must print one JSON document");

    let brew = detector_entry(&report, "homebrew_cache");
    assert_eq!(
        brew.get("status").and_then(|v| v.as_str()),
        Some("found"),
        "the succeeding `brew` shim prints a cache dir holding one file; got: {brew}"
    );
    assert_eq!(
        brew.get("candidates_found").and_then(|v| v.as_u64()),
        Some(1),
        "one file in the shimmed cache is one candidate; got: {brew}"
    );

    let failed: Vec<&serde_json::Value> = report["detectors"]
        .as_array()
        .expect("`detectors` must be an array")
        .iter()
        .filter(|e| e.get("status").and_then(|v| v.as_str()) == Some("failed"))
        .collect();
    assert!(
        failed.is_empty(),
        "no detector failed in this run; got {failed:?}"
    );
    assert_eq!(
        report.get("discovery_complete").and_then(|v| v.as_bool()),
        Some(true),
        "every detector answered, so discovery was complete; got:\n{stdout}"
    );

    fixture.cleanup();
}

/// AC 1/AC 3 at the wire: the `--progress-json` stream the menu-bar app
/// consumes carries each detector's outcome, not only a candidate count.
///
/// `candidates_found: 0` is what a failed probe and an absent tool had in
/// common, and it was all the stream said about either. A GUI reading this
/// stream had no way to tell "this detector found nothing" from "this
/// detector never answered", which is what let it render a partial candidate
/// list as the complete picture.
#[test]
fn progress_stream_carries_each_detectors_outcome_not_only_its_count() {
    let fixture = Fixture::new("horo1484-progress", true);
    let output = fixture.run(&["detect", "--json", "--progress-json"]);
    assert!(
        output.status.success(),
        "glomeris detect --json --progress-json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("every progress line must be JSON: {e}\nline: {l}"))
        })
        .collect();
    assert!(
        !events.is_empty(),
        "--progress-json must emit an NDJSON stream on stderr"
    );

    let finished = |detector: &str| -> serde_json::Value {
        events
            .iter()
            .find(|e| {
                e.get("phase").and_then(|v| v.as_str()) == Some("detector_finished")
                    && e.get("detector").and_then(|v| v.as_str()) == Some(detector)
            })
            .cloned()
            .unwrap_or_else(|| {
                panic!("no detector_finished event for {detector:?}; stream was:\n{stderr}")
            })
    };

    let brew = finished("homebrew_cache");
    let docker = finished("docker_build_cache");

    // Both report zero candidates — that is exactly why the count alone is
    // not enough information, and asserting it here keeps the point explicit.
    assert_eq!(
        brew.get("candidates_found").and_then(|v| v.as_u64()),
        Some(0)
    );
    assert_eq!(
        docker.get("candidates_found").and_then(|v| v.as_u64()),
        Some(0)
    );

    assert_eq!(
        brew.get("outcome").and_then(|v| v.as_str()),
        Some("failed"),
        "a failed probe must be reported as failed; got: {brew}"
    );
    assert!(
        brew.get("reason")
            .and_then(|v| v.as_str())
            .is_some_and(|r| !r.trim().is_empty()),
        "a failed probe must carry its reason; got: {brew}"
    );
    assert_eq!(
        docker.get("outcome").and_then(|v| v.as_str()),
        Some("tool_absent"),
        "an absent tool must not be reported as a failure; got: {docker}"
    );
    assert!(
        docker.get("reason").is_none(),
        "an absent tool has no failure reason; got: {docker}"
    );

    fixture.cleanup();
}

/// AC 2, human mode: `detect`'s human output does not claim an empty result
/// is a clean bill of health when a detector failed.
///
/// Uses no `--project-root`, so nothing is found, and the empty-list branch
/// — which used to print the bare words "no candidates discovered" — is the
/// one that runs.
#[test]
fn detect_human_mode_does_not_call_a_partial_search_a_clean_bill_of_health() {
    let fixture = Fixture::new("horo1484-human", true);

    // Deliberately bypasses `Fixture::run`, which always appends
    // `--project-root`: this case needs the empty-candidate-list branch.
    let output = Command::new(glomeris_bin())
        .arg("detect")
        .env("HOME", &fixture.home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fixture.shim_dir.display()),
        )
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");
    assert!(output.status.success(), "glomeris detect should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not a clean bill of health"),
        "with a failed detector and no candidates, the human output must say the \
         empty result is not a clean bill of health; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("no candidates discovered\n"),
        "the bare unqualified claim must not be printed when a detector failed; \
         got:\n{stdout}"
    );

    fixture.cleanup();
}
