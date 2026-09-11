use glomeris::{monitor, platform};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("daemon") => run_daemon_command(args.get(1).map(String::as_str)),
        Some("scan") => glomeris::scanner::run_scan_cli(&args[1..]),
        Some("status") => run_status_command(&args[1..]),
        Some("detect") => run_detect_command(&args[1..]),
        Some("explain") => run_explain_command(&args[1..]),
        Some("clean") => run_clean_command(&args[1..]),
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
        "usage: glomeris <daemon <install|uninstall|status|run>|scan|status [--json]|\
         detect [--json]|explain <resource_id_or_path> [--json]|\
         clean --dry-run [--target <resource_id_or_path>]|emergency|\
         free --target <N%|NB>>"
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
/// the current wall-clock time.
fn discover_and_classify_now() -> Vec<(
    glomeris::evidence::Evidence,
    glomeris::policy::PolicyDecision,
)> {
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::policy::PolicyConfig;
    use std::time::SystemTime;

    let ctx = DiscoveryContext::new(home_dir());
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

    let (_positionals, flags) = split_flags(args, &["--json"]);

    let ctx = DiscoveryContext::new(home_dir());
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

    let candidates = discover_and_classify_now();
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
    let (positionals, flags) = split_flags(args, &["--json"]);

    let Some(query) = positionals.first() else {
        eprintln!("glomeris explain: a resource id or path argument is required");
        print_usage();
        std::process::exit(2);
    };

    let candidates = discover_and_classify_now();
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
    let mut dry_run = false;
    let mut target: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--target" => {
                target = args.get(i + 1).map(String::as_str);
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
    let candidates = discover_and_classify_now();
    let actions = ActionRegistry::builtin();

    match glomeris::cli::build_clean_dry_run_report(&candidates, &actions, target) {
        Ok(report) => glomeris::cli::print_clean_dry_run_report(&report),
        Err(e) => {
            eprintln!("glomeris clean: {e}");
            std::process::exit(1);
        }
    }
}

fn run_free_command(args: &[String]) {
    let mut target_arg: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--target" {
            target_arg = args.get(i + 1).map(String::as_str);
            i += 2;
        } else {
            eprintln!("glomeris free: unrecognized argument '{}'", args[i]);
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

    free_run(target);
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

fn run_daemon_command(subcommand: Option<&str>) {
    match subcommand {
        Some("install") => daemon_install(),
        Some("uninstall") => daemon_uninstall(),
        Some("status") => daemon_status(),
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
fn daemon_install() {
    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon install: {e}");
            std::process::exit(1);
        }
    };
    match platform::macos::launchd::install(&plist_path, &current_exe_path()) {
        Ok(()) => println!("installed launch agent at {}", plist_path.display()),
        Err(e) => {
            eprintln!("glomeris daemon install: launchctl load failed: {e}");
            eprintln!(
                "plist was written to {} — retry `launchctl load -w` manually if needed",
                plist_path.display()
            );
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

#[cfg(target_os = "macos")]
fn daemon_status() {
    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon status: {e}");
            std::process::exit(1);
        }
    };
    let status = platform::macos::launchd::status(&plist_path);
    println!("plist installed: {}", status.plist_installed);
    println!("plist path: {}", status.plist_path.display());
    println!("loaded in launchd: {}", status.loaded);
}

#[cfg(target_os = "macos")]
fn daemon_run() {
    use monitor::{FilePersistence, PollConfig, SystemClock, ThresholdConfig};
    use platform::macos::{MacosFsStat, MacosNotifier};

    let history_path = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/history.tsv"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-history.tsv"));

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
        None,
        |outcome| {
            if let Err(e) = outcome {
                eprintln!("glomeris monitor: poll failed: {e}");
            }
        },
    );
}

#[cfg(target_os = "macos")]
fn free_run(target: glomeris::executor::recovery_loop::FreeTarget) {
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

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let discovery_ctx = DiscoveryContext::new(home_dir);

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
fn free_run(_target: glomeris::executor::recovery_loop::FreeTarget) {
    eprintln!("glomeris free: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_install() {
    eprintln!("glomeris daemon install: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_uninstall() {
    eprintln!("glomeris daemon uninstall: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_status() {
    eprintln!("glomeris daemon status: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_run() {
    eprintln!("glomeris daemon run: only supported on macOS");
    std::process::exit(1);
}
