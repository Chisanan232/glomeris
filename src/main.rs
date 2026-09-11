use glomeris::{monitor, platform};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("daemon") => run_daemon_command(args.get(1).map(String::as_str)),
        Some("scan") => glomeris::scanner::run_scan_cli(&args[1..]),
        Some("detect") => run_detect_command(),
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
    eprintln!("usage: glomeris <daemon <install|uninstall|status|run>|scan|detect>");
}

fn run_detect_command() {
    use glomeris::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    let ctx = DiscoveryContext::new(home_dir);
    let registry = DetectorRegistry::builtin();

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
