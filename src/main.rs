use glomeris::{monitor, platform};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") | Some("help") => {
            println!(
                "usage: glomeris <daemon <install [--force]|uninstall|status [--json]|run>|scan|status [--json]|\
                 detect [--project-root <path>]... [--json]|\
                 explain <resource_id_or_path> [--project-root <path>]... [--json]|\
                 clean --dry-run [--target <resource_id_or_path>] [--project-root <path>]...|\
                 llm-plan [--project-root <path>]... [--plan-file <path>] [--json]|\
                 emergency|\
                 free --target <N%|NB> [--project-root <path>]...>"
            );
        }
        Some("daemon") => run_daemon_command(&args[1..]),
        Some("scan") => glomeris::scanner::run_scan_cli(&args[1..]),
        Some("status") => run_status_command(&args[1..]),
        Some("detect") => run_detect_command(&args[1..]),
        Some("explain") => run_explain_command(&args[1..]),
        Some("clean") => run_clean_command(&args[1..]),
        Some("llm-plan") => run_llm_plan_command(&args[1..]),
        Some("emergency") => run_emergency_command(),
        Some("free") => run_free_command(&args[1..]),
        Some(other) => {
            eprintln!("glomeris: unknown command '{other}'");
            print_usage();
            std::process::exit(2);
        }
        None => {
            println!("glomeris {}", env!("CARGO_PKG_VERSION"));
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: glomeris <daemon <install [--force]|uninstall|status [--json]|run>|scan|status [--json]|\
         detect [--project-root <path>]... [--json]|\
         explain <resource_id_or_path> [--project-root <path>]... [--json]|\
         clean --dry-run [--target <resource_id_or_path>] [--project-root <path>]...|\
         emergency|\
         free --target <N%|NB> [--project-root <path>]...>"
    );
}

/// Parses a flat argument list into a positional-args list and a set of
/// bare `--flag` switches (no `--flag value` pairs handled here — callers
/// that need a valued flag, e.g. `--target`, parse that one explicitly
/// before calling this on what remains). Never panics on malformed input;
/// every unrecognized `--...` token is treated as a flag, and every other
/// token is positional.
fn split_flags<'a>(args: &'a [String], known_flags: &[&str]) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut positionals = Vec::new();
    let mut flags = Vec::new();
    for arg in args {
        if known_flags.contains(&arg.as_str()) {
            flags.push(arg.as_str());
        } else {
            positionals.push(arg.as_str());
        }
    }
    (positionals, flags)
}

fn print_json_or_exit(value: &impl serde::Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("glomeris: failed to render JSON report: {e}");
            std::process::exit(1);
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Builds the standard evidence-correlation-classification pipeline every
/// `detect`/`explain`/`clean` subcommand uses: real detectors, the real
/// `DefaultEvidenceCollector`, and the default policy config, evaluated at
/// the current wall-clock time. `project_roots` (HORO-957) is threaded into
/// the `DiscoveryContext` so the cargo/node detectors — which only ever scan
/// paths under `known_project_roots` — can actually find anything; it is
/// empty when the caller passed no `--project-root` flags, preserving the
/// prior behavior for anyone who doesn't use the new flag.
fn discover_and_classify_now(
    project_roots: Vec<PathBuf>,
) -> Vec<(
    glomeris::evidence::Evidence,
    glomeris::policy::PolicyDecision,
)> {
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::policy::PolicyConfig;
    use std::time::SystemTime;

    let ctx = DiscoveryContext::new(home_dir()).with_known_project_roots(project_roots);
    let registry = DetectorRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();
    let cfg = PolicyConfig::default();
    let now = SystemTime::now();

    glomeris::cli::discover_and_classify(&registry, &ctx, &collector, &cfg, now)
}

/// `glomeris status` — current disk pressure state (HORO-955).
#[cfg(target_os = "macos")]
fn run_status_command(args: &[String]) {
    use glomeris::monitor::{FsStat, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;

    let (_positionals, flags) = split_flags(args, &["--json"]);
    let fs_stat = MacosFsStat;
    let usage = match fs_stat.stat(std::path::Path::new("/")) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("glomeris status: failed to read filesystem usage: {e}");
            std::process::exit(1);
        }
    };

    let report = glomeris::cli::build_status_report(&usage, &ThresholdConfig::default());
    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_status_report(&report);
    }
}

#[cfg(not(target_os = "macos"))]
fn run_status_command(_args: &[String]) {
    eprintln!("glomeris status: only supported on macOS");
    std::process::exit(1);
}

/// Exit code used when [`glomeris::executor::lock::acquire_execution_lock`]
/// reports [`glomeris::executor::lock::LockError::AlreadyHeld`] — a
/// distinct "busy" signal (loosely following `sysexits.h`'s `EX_TEMPFAIL`)
/// rather than a generic failure, so a caller/script can tell "another
/// real execution is already in progress, retry later" apart from "this
/// invocation itself failed".
const EXIT_EXECUTION_LOCK_BUSY: i32 = 75;

/// Acquires the standalone HORO-1054 execution lock or exits with
/// [`EXIT_EXECUTION_LOCK_BUSY`]/a generic failure, printing `command_name`
/// in the error message. Shared by `run_emergency_command` and
/// `free_run` — the two existing real-execution entry points; a future
/// `execute` subcommand (HORO-1055) reuses the same
/// `glomeris::executor::lock::acquire_execution_lock` primitive.
#[cfg(target_os = "macos")]
fn acquire_execution_lock_or_exit(
    command_name: &str,
) -> glomeris::executor::lock::ExecutionLockGuard {
    use glomeris::executor::lock::{acquire_execution_lock, LockError};

    match acquire_execution_lock() {
        Ok(guard) => guard,
        Err(LockError::AlreadyHeld) => {
            eprintln!(
                "glomeris {command_name}: another glomeris execution is already in progress \
                 (execution lock busy) — try again shortly"
            );
            std::process::exit(EXIT_EXECUTION_LOCK_BUSY);
        }
        Err(LockError::Io(e)) => {
            eprintln!("glomeris {command_name}: failed to acquire the execution lock: {e}");
            std::process::exit(1);
        }
    }
}

/// `glomeris emergency` — the degraded-path recovery command (HORO-953).
/// Never touches network or an LLM provider; see
/// `glomeris::emergency`'s module docs for the full contract.
#[cfg(target_os = "macos")]
fn run_emergency_command() {
    use glomeris::actions::ActionRegistry;
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::emergency::run_emergency;
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::monitor::FilePersistence;
    use glomeris::platform::macos::MacosFsStat;
    use std::time::Duration;

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns.
    let _execution_lock = acquire_execution_lock_or_exit("emergency");

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    // Same history path `daemon_run` uses — emergency mode's own history
    // record is this tool's self-owned disposable state (see the
    // `emergency` module docs' step 1).
    let self_state_path = home_dir.join("Library/Application Support/Glomeris/history.tsv");

    let ctx = DiscoveryContext::new(home_dir);
    let registry = DetectorRegistry::builtin();
    let actions = ActionRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();
    let fs_stat = MacosFsStat;
    let persistence = FilePersistence::new(&self_state_path);

    let report = run_emergency(
        &fs_stat,
        &collector,
        &registry,
        &actions,
        &persistence,
        &ctx,
        &self_state_path,
        20,
        Duration::from_secs(30),
    );

    print!("{report}");
}

#[cfg(not(target_os = "macos"))]
fn run_emergency_command() {
    eprintln!("glomeris emergency: only supported on macOS");
    std::process::exit(1);
}

/// `glomeris detect` — per-detector discovery status, plus (HORO-955) a
/// per-candidate report line showing reclaimable bytes and policy
/// classification.
fn run_detect_command(args: &[String]) {
    use glomeris::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};

    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris detect: {e}");
            print_usage();
            std::process::exit(2);
        }
    };
    let (_positionals, flags) = split_flags(&remaining, &["--json"]);

    let ctx = DiscoveryContext::new(home_dir()).with_known_project_roots(project_roots.clone());
    let registry = DetectorRegistry::builtin();

    if !flags.contains(&"--json") {
        for (id, status) in registry.discover_all(&ctx) {
            match status {
                DetectorStatus::Found(evidence) => {
                    println!("{:<24} found ({} evidence)", id.0, evidence.len());
                }
                DetectorStatus::ToolAbsent => {
                    println!("{:<24} tool_absent", id.0);
                }
                DetectorStatus::Failed(reason) => {
                    println!("{:<24} failed: {reason}", id.0);
                }
            }
        }
    }

    let candidates = discover_and_classify_now(project_roots);
    let report = glomeris::cli::build_detect_report(&candidates);

    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_detect_report(&report);
    }
}

/// `glomeris explain <resource_id_or_path>` — full evidence-and-policy
/// picture for exactly one resource (HORO-955).
fn run_explain_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris explain: {e}");
            print_usage();
            std::process::exit(2);
        }
    };
    let (positionals, flags) = split_flags(&remaining, &["--json"]);

    let Some(query) = positionals.first() else {
        eprintln!("glomeris explain: a resource id or path argument is required");
        print_usage();
        std::process::exit(2);
    };

    let candidates = discover_and_classify_now(project_roots);
    let Some((ev, decision)) = glomeris::cli::find_candidate(query, &candidates) else {
        eprintln!("glomeris explain: no discoverable candidate matches '{query}'");
        std::process::exit(1);
    };

    let report = glomeris::cli::build_explain_report(ev, decision);
    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_explain_report(&report);
    }
}

/// `glomeris clean --dry-run [--target <resource_id_or_path>]` — renders
/// what would be cleaned, without executing anything (HORO-955). There is
/// no non-dry-run execution path on this subcommand — see the PR's "Known
/// limitations": real destructive execution stays `free --target`'s job.
fn run_clean_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris clean: {e}");
            print_usage();
            std::process::exit(2);
        }
    };

    let mut dry_run = false;
    let mut target: Option<&str> = None;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--target" => {
                target = remaining.get(i + 1).map(String::as_str);
                if target.is_none() {
                    eprintln!("glomeris clean: --target requires a value");
                    std::process::exit(2);
                }
                i += 2;
            }
            other => {
                eprintln!("glomeris clean: unrecognized argument '{other}'");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    if !dry_run {
        eprintln!(
            "glomeris clean: --dry-run is required; real destructive cleanup is not \
             implemented by this subcommand (use `glomeris free --target <N%|NB>` instead)"
        );
        std::process::exit(2);
    }

    use glomeris::actions::ActionRegistry;
    let candidates = discover_and_classify_now(project_roots);
    let actions = ActionRegistry::builtin();

    match glomeris::cli::build_clean_dry_run_report(&candidates, &actions, target) {
        Ok(report) => glomeris::cli::print_clean_dry_run_report(&report),
        Err(e) => {
            eprintln!("glomeris clean: {e}");
            std::process::exit(1);
        }
    }
}

/// `glomeris llm-plan [--project-root <path>]... [--plan-file <path>]
/// [--json]` — ADVISORY, NON-EXECUTING BYOK LLM suggestion surface
/// (HORO-1008). Never constructs a [`glomeris::policy::Approval`] and
/// never calls [`glomeris::policy::approval::authorize`] or
/// [`glomeris::executor::execute`] — see
/// [`glomeris::cli::build_llm_plan_report`]'s doc comment.
///
/// Without `--plan-file`, credentials are read only from
/// `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL` via
/// [`glomeris::actions::llm::provider_from_env`] — never accepted as a CLI
/// argument, to keep a key out of `ps`/shell history.
fn run_llm_plan_command(args: &[String]) {
    use glomeris::actions::llm::{provider_from_env, FilePlanProvider};
    use glomeris::actions::ActionRegistry;

    let (project_roots, after_roots) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris llm-plan: {e}");
            print_usage();
            std::process::exit(2);
        }
    };
    let (plan_file, remaining) = match glomeris::cli::extract_plan_file(&after_roots) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris llm-plan: {e}");
            print_usage();
            std::process::exit(2);
        }
    };

    let mut json = false;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--json" => {
                json = true;
                i += 1;
            }
            other => {
                // Match on the flag name only — never the whole token — so a
                // `--api-key=<secret>` invocation can't echo the secret to
                // stderr the way printing `other`/`remaining[i]` verbatim
                // would. `credential_flag_name` isolates the flag name for
                // both the space-separated (`--api-key sk-...`) and
                // `=`-joined (`--api-key=sk-...`) forms without ever
                // touching the value.
                let flag_name = glomeris::cli::credential_flag_name(other);
                if matches!(flag_name, "--api-key" | "--key" | "--token") {
                    eprintln!(
                        "glomeris llm-plan: unrecognized argument '{flag_name}' — read the key \
                         from $GLOMERIS_LLM_API_KEY; passing a key in argv exposes it to ps and \
                         shell history"
                    );
                    std::process::exit(2);
                }
                eprintln!("glomeris llm-plan: unrecognized argument '{other}'");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    let candidates = discover_and_classify_now(project_roots);
    let actions = ActionRegistry::builtin();

    let report = match plan_file {
        Some(path) => {
            let provider = FilePlanProvider { path };
            glomeris::cli::build_llm_plan_report(&candidates, &actions, &provider)
        }
        None => match provider_from_env() {
            Ok(provider) => glomeris::cli::build_llm_plan_report(&candidates, &actions, &provider),
            Err(_) => {
                eprintln!(
                    "glomeris llm-plan: missing LLM configuration — set GLOMERIS_LLM_API_KEY, \
                     GLOMERIS_LLM_BASE_URL, and GLOMERIS_LLM_MODEL, or pass \
                     --plan-file <path> instead"
                );
                std::process::exit(2);
            }
        },
    };

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_llm_plan_report(&report);
    }

    if report.provider_error.is_some() {
        std::process::exit(1);
    }
}

fn run_free_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris free: {e}");
            print_usage();
            std::process::exit(2);
        }
    };

    let mut target_arg: Option<&str> = None;
    let mut i = 0;
    while i < remaining.len() {
        if remaining[i] == "--target" {
            target_arg = remaining.get(i + 1).map(String::as_str);
            i += 2;
        } else {
            eprintln!("glomeris free: unrecognized argument '{}'", remaining[i]);
            print_usage();
            std::process::exit(2);
        }
    }

    let target_arg = match target_arg {
        Some(t) => t,
        None => {
            eprintln!("glomeris free: --target is required");
            print_usage();
            std::process::exit(2);
        }
    };

    let target = match glomeris::executor::recovery_loop::parse_free_target(target_arg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("glomeris free: {e}");
            std::process::exit(2);
        }
    };

    free_run(target, project_roots);
}

fn print_recovery_report(report: &glomeris::executor::recovery_loop::RecoveryReport) {
    use glomeris::reporting::human_bytes;

    println!("stop reason:            {:?}", report.stop_reason);
    println!("iterations run:         {}", report.iterations_run);
    println!("actions executed:       {}", report.actions_executed);
    println!(
        "actions declined/skipped: {}",
        report.actions_declined_or_skipped
    );
    // `total_bytes_freed` is a MEASURED total (see
    // `RecoveryReport::total_bytes_freed`'s own doc comment: "Sum of
    // ACTUAL (not expected) reclaimed bytes"), never an estimate — labeled
    // explicitly as such so this never reads as the same kind of number as
    // a detector's `reclaimable_bytes` estimate.
    println!(
        "bytes freed (measured): {} ({} bytes)",
        human_bytes(report.total_bytes_freed),
        report.total_bytes_freed
    );
    println!(
        "free before:            {} ({} bytes)",
        human_bytes(report.started_free_bytes),
        report.started_free_bytes
    );
    println!(
        "free after:             {} ({} bytes)",
        human_bytes(report.final_free_bytes),
        report.final_free_bytes
    );
}

fn run_daemon_command(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("install") => {
            let mut force = false;
            for arg in &args[1..] {
                if arg == "--force" {
                    force = true;
                } else {
                    eprintln!("glomeris daemon install: unrecognized argument '{arg}'");
                    std::process::exit(2);
                }
            }
            daemon_install(force);
        }
        Some("uninstall") => daemon_uninstall(),
        Some("status") => daemon_status(&args[1..]),
        Some("run") => daemon_run(),
        Some(other) => {
            eprintln!("glomeris daemon: unknown subcommand '{other}'");
            print_usage();
            std::process::exit(2);
        }
        None => {
            print_usage();
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
fn current_exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("glomeris"))
}

#[cfg(target_os = "macos")]
fn daemon_install(force: bool) {
    use platform::macos::launchd::InstallOutcome;

    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon install: {e}");
            std::process::exit(1);
        }
    };
    match platform::macos::launchd::install(&plist_path, &current_exe_path(), force) {
        Ok(InstallOutcome::Installed) => {
            println!("installed launch agent at {}", plist_path.display());
        }
        Ok(InstallOutcome::AlreadyUpToDate) => {
            println!(
                "launch agent already up to date at {}",
                plist_path.display()
            );
        }
        Ok(InstallOutcome::Repaired) => {
            println!(
                "repaired launch agent at {} (preserved existing StartInterval)",
                plist_path.display()
            );
        }
        Ok(InstallOutcome::RefusedNeedsForce) => {
            eprintln!(
                "refused: existing plist at {} has unrecognized customization — rerun with \
                 --force to overwrite (a backup will be taken first)",
                plist_path.display()
            );
            std::process::exit(2);
        }
        Ok(InstallOutcome::AbortedConcurrentModification) => {
            eprintln!(
                "aborted: {} was modified concurrently, no changes made — retry",
                plist_path.display()
            );
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("glomeris daemon install: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "macos")]
fn daemon_uninstall() {
    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon uninstall: {e}");
            std::process::exit(1);
        }
    };
    match platform::macos::launchd::uninstall(&plist_path) {
        Ok(()) => println!("uninstalled launch agent"),
        Err(e) => {
            eprintln!("glomeris daemon uninstall: {e}");
            std::process::exit(1);
        }
    }
}

/// Same `Library/Application Support/Glomeris/heartbeat.json` path
/// `daemon_run` writes to (HORO-1044) — shared here so `daemon status
/// --json` (HORO-1045) reads back exactly what the poll loop wrote.
#[cfg(target_os = "macos")]
fn heartbeat_path() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/heartbeat.json"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-heartbeat.json"))
}

#[cfg(target_os = "macos")]
fn daemon_status(args: &[String]) {
    let (_positionals, flags) = split_flags(args, &["--json"]);

    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon status: {e}");
            std::process::exit(1);
        }
    };
    let status = platform::macos::launchd::status(&plist_path);
    let heartbeat = monitor::read_heartbeat(&heartbeat_path());
    let now = monitor::persistence::unix_now_secs();

    let report = glomeris::cli::build_daemon_status_report(
        status.plist_installed,
        &status.plist_path,
        status.loaded,
        heartbeat.as_ref(),
        now,
    );

    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_daemon_status_report(&report);
    }
}

#[cfg(target_os = "macos")]
fn daemon_run() {
    use monitor::{FilePersistence, PollConfig, SystemClock, ThresholdConfig};
    use platform::macos::{MacosFsStat, MacosNotifier};

    let history_path = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/history.tsv"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-history.tsv"));
    // Same `Library/Application Support/Glomeris/` directory as
    // `history_path` above — see `monitor::run`'s doc comment (HORO-1044);
    // `daemon_status`'s `heartbeat_path()` reads this back for `daemon
    // status --json` (HORO-1045).
    let heartbeat_path = heartbeat_path();

    let config = PollConfig::new("/");
    let thresholds = ThresholdConfig::default();
    let clock = SystemClock;
    let fs_stat = MacosFsStat;
    let notifier = MacosNotifier;
    let persistence = FilePersistence::new(history_path);

    monitor::run(
        &config,
        &thresholds,
        &clock,
        &fs_stat,
        &notifier,
        &persistence,
        &heartbeat_path,
        None,
        |outcome| {
            if let Err(e) = outcome {
                eprintln!("glomeris monitor: poll failed: {e}");
            }
        },
    );
}

#[cfg(target_os = "macos")]
fn free_run(target: glomeris::executor::recovery_loop::FreeTarget, project_roots: Vec<PathBuf>) {
    use glomeris::actions::ActionRegistry;
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::executor::recovery_loop::{
        run as run_recovery_loop, RecoveryConfig, SystemWallClock,
    };
    use glomeris::monitor::SystemClock;
    use glomeris::platform::macos::MacosFsStat;
    use glomeris::policy::PolicyConfig;
    use std::time::Duration;

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns.
    let _execution_lock = acquire_execution_lock_or_exit("free");

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let discovery_ctx = DiscoveryContext::new(home_dir).with_known_project_roots(project_roots);

    let config = RecoveryConfig {
        target,
        max_iterations: 100,
        max_actions: 100,
        max_duration: Duration::from_secs(600),
        // No interactive prompt in this MVP — Ask candidates are
        // reported as declined/skipped rather than executed. See
        // RecoveryConfig::auto_approve_ask's doc comment.
        auto_approve_ask: false,
    };

    let fs_stat = MacosFsStat;
    let collector = DefaultEvidenceCollector::default();
    let detector_registry = DetectorRegistry::builtin();
    let action_registry = ActionRegistry::builtin();
    let clock = SystemClock;
    let wall_clock = SystemWallClock;
    let policy_cfg = PolicyConfig::default();

    let report = run_recovery_loop(
        &config,
        &fs_stat,
        &collector,
        &detector_registry,
        &action_registry,
        &clock,
        &wall_clock,
        &policy_cfg,
        std::path::Path::new("/"),
        &discovery_ctx,
    );

    print_recovery_report(&report);
}

#[cfg(not(target_os = "macos"))]
fn free_run(_target: glomeris::executor::recovery_loop::FreeTarget, _project_roots: Vec<PathBuf>) {
    eprintln!("glomeris free: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_install(_force: bool) {
    eprintln!("glomeris daemon install: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_uninstall() {
    eprintln!("glomeris daemon uninstall: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_status(_args: &[String]) {
    eprintln!("glomeris daemon status: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_run() {
    eprintln!("glomeris daemon run: only supported on macOS");
    std::process::exit(1);
}
