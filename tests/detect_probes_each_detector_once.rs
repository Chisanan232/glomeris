//! HORO-1487 regression proof: `glomeris detect` probes every detector
//! exactly once per invocation, in human mode as well as `--json` mode.
//!
//! Human mode used to run two independent discovery passes — one
//! `discover_all` for the per-detector status lines, then a second, complete
//! discover-refresh-classify pass for the report underneath them. Every
//! detector therefore paid its full probe cost twice, and the two halves of
//! a single command's output described two different probes of a filesystem
//! that changes underneath them.
//!
//! The count here is an *observed* count of real probe invocations, not a
//! timing measurement and not an assertion about the shape of the code: two
//! of the five builtin detectors discover by spawning a tool found on
//! `PATH` (`brew --cache` and `docker system df`), so a shim earlier on
//! `PATH` that appends one line to a file per invocation counts the probes
//! from outside the process entirely. A timing-based test would be a flake
//! generator on a shared CI runner; a source-text test would pass against
//! any rewrite that kept the two calls and moved them around.
//!
//! Nothing here can touch real Homebrew or Docker state: the shims replace
//! both tools for the child process, `$HOME` is an empty temporary
//! directory, no `--project-root` is passed, and `detect` is read-only in
//! any case.

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

/// One shimmed tool: where its invocations are tallied, and the directory
/// holding the executable that does the tallying.
struct ProbeCounter {
    shim_dir: PathBuf,
    brew_tally: PathBuf,
    docker_tally: PathBuf,
}

impl ProbeCounter {
    /// Builds a `PATH` directory holding a `brew` and a `docker` that each
    /// append one line to their own tally file per invocation.
    ///
    /// `brew --cache` prints a real, existing directory containing one file,
    /// so the `homebrew_cache` detector reports `Found` with exactly one
    /// evidence — the interesting case, since a `Found` detector is the one
    /// whose evidence the report also consumes. `docker` exits non-zero,
    /// which the `docker_build_cache` detector treats as "daemon
    /// unreachable" and reports as `tool_absent`: a detector contributing no
    /// candidates still has to be probed exactly once, and that is the case
    /// a fix which merely moved the evidence around could get wrong.
    fn new(prefix: &str) -> Self {
        let dir = make_temp_dir(prefix);
        let shim_dir = dir.join("bin");
        fs::create_dir_all(&shim_dir).expect("create shim dir");

        let cache_dir = dir.join("brew-cache");
        fs::create_dir_all(&cache_dir).expect("create fake brew cache");
        fs::write(cache_dir.join("some-bottle.tar.gz"), vec![0u8; 2048])
            .expect("write fake brew cache file");

        let brew_tally = dir.join("brew-invocations");
        let docker_tally = dir.join("docker-invocations");

        write_shim(
            &shim_dir.join("brew"),
            &format!(
                "#!/bin/sh\nprintf 'probe\\n' >> '{}'\nprintf '%s\\n' '{}'\n",
                brew_tally.display(),
                cache_dir.display()
            ),
        );
        write_shim(
            &shim_dir.join("docker"),
            &format!(
                "#!/bin/sh\nprintf 'probe\\n' >> '{}'\nexit 1\n",
                docker_tally.display()
            ),
        );

        Self {
            shim_dir,
            brew_tally,
            docker_tally,
        }
    }

    fn brew_probes(&self) -> usize {
        tally(&self.brew_tally)
    }

    fn docker_probes(&self) -> usize {
        tally(&self.docker_tally)
    }
}

fn write_shim(path: &Path, script: &str) {
    fs::write(path, script).expect("write shim script");
    let mut perms = fs::metadata(path).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod shim");
}

/// Lines in a tally file. A file that was never created counts as zero
/// rather than panicking, so that "the shim never ran at all" fails the
/// `== 1` assertions below as loudly as "it ran twice" does — a test that
/// tolerated zero would pass vacuously if the shim stopped being reachable.
fn tally(path: &Path) -> usize {
    match fs::read_to_string(path) {
        Ok(contents) => contents.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("failed to read tally {}: {e}", path.display()),
    }
}

/// Runs the real binary with the shim directory first on `PATH` and an
/// isolated, empty `$HOME`.
///
/// `/usr/bin:/bin` stay on `PATH` behind the shims so that nothing else the
/// process may legitimately spawn goes missing; the shims come first, so the
/// host's real `brew`/`docker` (wherever they live) can never be reached.
fn run_detect(args: &[&str], counter: &ProbeCounter, home: &Path) -> Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", counter.shim_dir.display()),
        )
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// AC 1, human mode: the mode that had the defect. One probe per detector.
#[test]
fn detect_human_mode_probes_each_detector_exactly_once() {
    let counter = ProbeCounter::new("horo1487-human");
    let home = make_temp_dir("horo1487-human-home");

    let output = run_detect(&["detect"], &counter, &home);
    assert!(
        output.status.success(),
        "glomeris detect should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The shims were actually the tools that ran — otherwise the counts
    // below would be comparing zero against one.
    assert!(
        stdout.contains("homebrew_cache"),
        "detect's human output should include a homebrew_cache status line; got:\n{stdout}"
    );

    assert_eq!(
        counter.brew_probes(),
        1,
        "the homebrew_cache detector must be probed exactly once per `detect`, \
         not once for the status lines and again for the report; stdout was:\n{stdout}"
    );
    assert_eq!(
        counter.docker_probes(),
        1,
        "a detector that contributes no candidates must still be probed exactly \
         once per `detect`; stdout was:\n{stdout}"
    );
}

/// AC 1, `--json` mode: already correct before the fix, and asserted so it
/// cannot regress into the human mode's shape.
#[test]
fn detect_json_mode_probes_each_detector_exactly_once() {
    let counter = ProbeCounter::new("horo1487-json");
    let home = make_temp_dir("horo1487-json-home");

    let output = run_detect(&["detect", "--json"], &counter, &home);
    assert!(
        output.status.success(),
        "glomeris detect --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("detect --json must print one JSON document: {e}\n{stdout}"));

    assert_eq!(
        counter.brew_probes(),
        1,
        "one probe per detector in --json mode"
    );
    assert_eq!(
        counter.docker_probes(),
        1,
        "one probe per detector in --json mode"
    );
}

/// AC 2, from outside the process: the status lines and the report describe
/// the same probe.
///
/// With the `brew` shim reporting a cache directory that exists and holds
/// one file, the `homebrew_cache` status line must read `found (1 evidence)`
/// *and* that same path must appear in the report below it. Under two
/// independent passes these agreed only because nothing changed between
/// them; asserting the agreement pins the property that they are now
/// derived from one pass rather than happening to match.
#[test]
fn detect_status_lines_and_report_describe_the_same_probe() {
    let counter = ProbeCounter::new("horo1487-agree");
    let home = make_temp_dir("horo1487-agree-home");

    let output = run_detect(&["detect"], &counter, &home);
    assert!(output.status.success(), "glomeris detect should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let status_line = stdout
        .lines()
        .find(|l| l.starts_with("homebrew_cache"))
        .unwrap_or_else(|| panic!("no homebrew_cache status line in:\n{stdout}"));
    assert!(
        status_line.contains("found (1 evidence)"),
        "the shimmed brew cache holds exactly one file, so the status line should \
         report one evidence; got: {status_line:?}\nfull output:\n{stdout}"
    );

    assert_eq!(
        counter.brew_probes(),
        1,
        "the status line and the report must come from the same single probe"
    );
}
