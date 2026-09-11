//! `launchd` user-agent lifecycle: plist generation, install, uninstall,
//! status.
//!
//! Everything here runs at the per-user level (`~/Library/LaunchAgents`,
//! `launchctl ... gui/<uid>` semantics via plain `load`/`unload`) — no root
//! required, and none of this writes to a system-level LaunchDaemons
//! location.
//!
//! Known CI limitation: only plist generation and path/file logic are unit
//! tested here. Actually asking `launchd` to load/run the agent needs a
//! real macOS user session and is not exercised by `cargo test` — see the
//! PR's "Known limitations" section.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The `launchd` label this daemon registers under.
pub const LABEL: &str = "com.glomeris.monitor";

/// Default poll interval `launchd` is told to use when relaunching the
/// daemon (`StartInterval`), in seconds.
pub const DEFAULT_START_INTERVAL_SECS: u64 = 60;

/// Renders the `launchd` plist XML for running `program_path daemon run`
/// every `start_interval_secs` seconds. Pure and fully unit-testable: no
/// filesystem or process I/O.
pub fn generate_plist(program_path: &Path, start_interval_secs: u64) -> String {
    let program = xml_escape(&program_path.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
        <string>daemon</string>
        <string>run</string>
    </array>
    <key>StartInterval</key>
    <integer>{interval}</integer>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/glomeris-monitor.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/glomeris-monitor.err.log</string>
</dict>
</plist>
"#,
        label = LABEL,
        program = program,
        interval = start_interval_secs,
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Resolves the per-user `LaunchAgents` plist path for this daemon, using
/// `$HOME`. Returns an error rather than panicking if `HOME` is unset.
pub fn default_plist_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// Writes the generated plist to `plist_path`, creating parent directories
/// as needed. Pure file I/O, no `launchctl` invocation — kept separate so
/// it can be unit tested against a temp directory.
pub fn write_plist(
    plist_path: &Path,
    program_path: &Path,
    start_interval_secs: u64,
) -> io::Result<()> {
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        plist_path,
        generate_plist(program_path, start_interval_secs),
    )
}

/// Installs the launch agent: writes the plist, then asks `launchctl` to
/// load it. `launchctl` failures are surfaced as an error but the plist
/// file itself is left in place (so `status`/retry can still see it) —
/// only the final `launchctl load` result is fallible in a way that needs
/// a real macOS session to succeed.
pub fn install(plist_path: &Path, program_path: &Path) -> io::Result<()> {
    write_plist(plist_path, program_path, DEFAULT_START_INTERVAL_SECS)?;
    run_launchctl(&["load", "-w", &plist_path.display().to_string()])
}

/// Uninstalls the launch agent: asks `launchctl` to unload it (best-effort
/// — an already-unloaded agent reports an error from `launchctl` that we
/// don't treat as fatal here) and removes the plist file if present.
pub fn uninstall(plist_path: &Path) -> io::Result<()> {
    let _ = run_launchctl(&["unload", "-w", &plist_path.display().to_string()]);
    if plist_path.exists() {
        std::fs::remove_file(plist_path)?;
    }
    Ok(())
}

/// Current on-disk/launchd status of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    pub plist_installed: bool,
    pub plist_path: PathBuf,
    /// `true` if `launchctl list <label>` reported the job as loaded.
    /// Always `false` (never an error) when `launchctl` itself cannot be
    /// run, e.g. in a non-macOS CI sandbox.
    pub loaded: bool,
}

/// Reports whether the plist is installed and whether `launchctl`
/// currently considers the job loaded. Never errors: an unreachable
/// `launchctl` is reported as `loaded: false`, not surfaced as failure.
pub fn status(plist_path: &Path) -> DaemonStatus {
    let plist_installed = plist_path.exists();
    let loaded = run_launchctl(&["list", LABEL]).is_ok();
    DaemonStatus {
        plist_installed,
        plist_path: plist_path.to_path_buf(),
        loaded,
    }
}

fn run_launchctl(args: &[&str]) -> io::Result<()> {
    let status = Command::new("launchctl").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "launchctl {args:?} exited with status: {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_plist_contains_label_and_program_path() {
        let xml = generate_plist(Path::new("/usr/local/bin/glomeris"), 42);
        assert!(xml.contains(LABEL));
        assert!(xml.contains("/usr/local/bin/glomeris"));
        assert!(xml.contains("<integer>42</integer>"));
        assert!(xml.starts_with("<?xml"));
    }

    #[test]
    fn generate_plist_escapes_xml_special_characters_in_path() {
        let xml = generate_plist(Path::new("/tmp/a&b<c>\"d"), 1);
        assert!(!xml.contains("a&b<c>\"d"));
        assert!(xml.contains("a&amp;b&lt;c&gt;&quot;d"));
    }

    #[test]
    fn write_plist_creates_parent_dirs_and_readable_file() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-launchd-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let plist_path = dir.join("nested").join(format!("{LABEL}.plist"));

        write_plist(&plist_path, Path::new("/bin/echo"), 60).expect("write_plist should succeed");
        let contents = std::fs::read_to_string(&plist_path).unwrap();
        assert!(contents.contains("/bin/echo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reports_not_installed_for_missing_plist() {
        let missing = std::env::temp_dir().join("glomeris-launchd-definitely-missing.plist");
        let s = status(&missing);
        assert!(!s.plist_installed);
    }

    #[test]
    fn install_and_uninstall_do_not_panic_even_without_real_launchd_access() {
        // This exercises the full install/uninstall path against a temp
        // plist location. `launchctl load`/`unload` may fail in a sandboxed
        // CI environment (or be entirely absent on a non-macOS runner) —
        // that failure is allowed and asserted-on here, not a panic. Real
        // launchd scheduling is not verified; see PR known limitations.
        let dir = std::env::temp_dir().join(format!(
            "glomeris-launchd-lifecycle-test-{}",
            std::process::id()
        ));
        let plist_path = dir.join(format!("{LABEL}.plist"));

        let install_result = install(&plist_path, Path::new("/bin/echo"));
        // Whether or not launchctl succeeded, the plist file must exist.
        assert!(
            plist_path.exists(),
            "plist must be written regardless of launchctl outcome"
        );
        let _ = install_result;

        let uninstall_result = uninstall(&plist_path);
        assert!(
            uninstall_result.is_ok(),
            "uninstall must not fail even if launchctl unload failed"
        );
        assert!(
            !plist_path.exists(),
            "plist file must be removed by uninstall"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_plist_path_uses_home_and_label() {
        let path = default_plist_path().expect("HOME should be set in test environment");
        assert!(path.to_string_lossy().contains("Library/LaunchAgents"));
        assert!(path.to_string_lossy().ends_with(&format!("{LABEL}.plist")));
    }
}
