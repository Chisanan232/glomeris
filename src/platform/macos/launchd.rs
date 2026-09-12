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
use std::time::SystemTime;

/// The `launchd` label this daemon registers under.
pub const LABEL: &str = "com.glomeris.monitor";

/// Default poll interval `launchd` is told to use when relaunching the
/// daemon (`StartInterval`), in seconds.
pub const DEFAULT_START_INTERVAL_SECS: u64 = 60;

/// Renders the `launchd` plist XML for running `program_path daemon run`
/// every `start_interval_secs` seconds, redirecting stdout/stderr into
/// `log_dir`. Pure and fully unit-testable: no filesystem or process I/O.
pub fn generate_plist(program_path: &Path, start_interval_secs: u64, log_dir: &Path) -> String {
    let program = xml_escape(&program_path.display().to_string());
    let stdout_log = xml_escape(&log_dir.join("monitor.log").display().to_string());
    let stderr_log = xml_escape(&log_dir.join("monitor.err.log").display().to_string());
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
    <string>{stdout_log}</string>
    <key>StandardErrorPath</key>
    <string>{stderr_log}</string>
</dict>
</plist>
"#,
        label = LABEL,
        program = program,
        interval = start_interval_secs,
        stdout_log = stdout_log,
        stderr_log = stderr_log,
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

/// Resolves the per-user log directory this daemon's `launchd` job writes
/// stdout/stderr into, using `$HOME`. Returns an error rather than
/// panicking if `HOME` is unset. Mirrors `default_plist_path()`.
pub fn default_log_dir() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Logs")
        .join("Glomeris"))
}

/// Best-effort extraction of the `<integer>` value immediately following
/// `<key>StartInterval</key>` in `xml`. Returns `None` if the file can't
/// be read or doesn't match the expected shape — never errors the
/// caller; this is advisory, not authoritative parsing.
///
/// Refuses (returns `None`) if `<key>StartInterval</key>` occurs more than
/// once in the document: the real plists this tool generates have exactly
/// one such marker, so more than one is a signal that either the document
/// is adversarial (a `ProgramArguments` string embedding the literal
/// marker text to fool a naive split) or structurally not what this tool
/// expects — either way, guessing which occurrence is "the real one" is
/// worse than declining to extract at all.
fn extract_start_interval_secs(xml: &str) -> Option<u64> {
    const MARKER: &str = "<key>StartInterval</key>";
    if xml.matches(MARKER).count() != 1 {
        return None;
    }
    let after_key = xml.split(MARKER).nth(1)?;
    let after_open = after_key.split("<integer>").nth(1)?;
    let value = after_open.split("</integer>").next()?;
    value.trim().parse::<u64>().ok()
}

/// Best-effort extraction of the first `<string>` inside the
/// `ProgramArguments` array (the program path) — used only to build the
/// "what would this tool have generated, holding the interval fixed"
/// comparison below, never trusted as an executable path on its own.
///
/// Refuses (returns `None`) if `<key>ProgramArguments</key>` occurs more
/// than once in the document, for the same reason as
/// [`extract_start_interval_secs`]: a single expected marker is a
/// necessary precondition for the split-based extraction to be trusted.
fn extract_program_path(xml: &str) -> Option<String> {
    const MARKER: &str = "<key>ProgramArguments</key>";
    if xml.matches(MARKER).count() != 1 {
        return None;
    }
    let after_key = xml.split(MARKER).nth(1)?;
    let after_array = after_key.split("<array>").nth(1)?;
    let after_string = after_array.split("<string>").nth(1)?;
    let value = after_string.split("</string>").next()?;
    Some(value.to_string())
}

/// Writes `contents` to `path` atomically: writes to a sibling temp file
/// in the same directory (so the final rename is on the same filesystem
/// and therefore atomic), then `fs::rename`s over `path`. Cleans up the
/// temp file on any failure before the rename.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let tmp_path = path.with_extension("plist.tmp");
    if let Err(e) = std::fs::write(&tmp_path, contents) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Writes the generated plist to `plist_path`, creating parent directories
/// as needed. Pure file I/O plus the atomic-write helper, no `launchctl`
/// invocation — kept separate so it can be unit tested against a temp
/// directory.
pub fn write_plist(
    plist_path: &Path,
    program_path: &Path,
    start_interval_secs: u64,
    log_dir: &Path,
) -> io::Result<()> {
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(
        plist_path,
        &generate_plist(program_path, start_interval_secs, log_dir),
    )
}

/// A cheap snapshot of a file's identity used to detect whether it changed
/// between two points in time without holding any kind of lock. `None` for
/// `modified` means the filesystem/platform couldn't report an mtime; the
/// comparison then falls back to length alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

/// Captures a `FileFingerprint` for `path`. `None` if the path doesn't
/// exist or its metadata can't be read.
fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileFingerprint {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// Returns `true` if `path`'s current on-disk fingerprint differs from
/// `before` (including the case where the file has since been deleted, or
/// didn't exist before but does now) — the light TOCTOU guard used by
/// `install()` to detect a concurrent modification between its initial
/// read and its eventual write.
fn file_changed_since(path: &Path, before: Option<FileFingerprint>) -> bool {
    fingerprint(path) != before
}

/// The result of calling [`install`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// No plist existed before this call.
    Installed,
    /// A plist existed and was byte-identical to what a completely fresh
    /// install with the current inputs (default `StartInterval`, current
    /// program path) would generate — nothing to preserve, nothing to
    /// refresh, nothing was written.
    AlreadyUpToDate,
    /// A plist existed and differed ONLY in StartInterval (preserved)
    /// and/or the program path (always refreshed to the current
    /// caller-supplied path, since that's the actual point of calling
    /// install again — pointing at a rebuilt/moved binary) — rewritten
    /// without needing `force`.
    Repaired,
    /// A plist existed with some OTHER difference (e.g. a hand-added or
    /// hand-changed key this tool doesn't manage) and `force` was not
    /// set — refused, zero mutation. Caller should report this plainly,
    /// not treat it as success.
    RefusedNeedsForce,
    /// A plist existed, was modified by someone else between this
    /// function's read and its write, and the write was aborted rather
    /// than risk clobbering that concurrent change. Zero mutation.
    AbortedConcurrentModification,
}

/// Installs the launch agent: writes the plist (preserving a hand-edited
/// `StartInterval` and refusing to clobber any other hand-edited
/// customization unless `force` is set — see [`InstallOutcome`]), then
/// asks `launchctl` to load it. `launchctl` failures are surfaced as an
/// error but the plist file itself is left in place (so `status`/retry can
/// still see it) — only the final `launchctl load` result is fallible in a
/// way that needs a real macOS session to succeed.
pub fn install(plist_path: &Path, program_path: &Path, force: bool) -> io::Result<InstallOutcome> {
    let log_dir = default_log_dir()?;
    let outcome = install_impl(plist_path, program_path, force, &log_dir)?;
    match outcome {
        InstallOutcome::Installed | InstallOutcome::AlreadyUpToDate | InstallOutcome::Repaired => {
            run_launchctl(&["load", "-w", &plist_path.display().to_string()])?;
        }
        InstallOutcome::RefusedNeedsForce | InstallOutcome::AbortedConcurrentModification => {}
    }
    Ok(outcome)
}

/// The actual `install()` algorithm, parameterized on `log_dir` so unit
/// tests can point it at a temp directory instead of touching the real
/// `$HOME` (`install()`'s public 3-arg signature resolves `log_dir` from
/// `$HOME` via `default_log_dir()`, which is what production/CLI callers
/// go through; tests call this helper directly to stay hermetic and to
/// avoid mutating the process-global `HOME` env var across parallel
/// tests). Does not call `launchctl` — that stays in `install()` so it
/// runs uniformly across every outcome that actually wrote the file.
fn install_impl(
    plist_path: &Path,
    program_path: &Path,
    force: bool,
    log_dir: &Path,
) -> io::Result<InstallOutcome> {
    install_impl_with_hook(
        plist_path,
        program_path,
        force,
        log_dir,
        TestHooks::default(),
    )
}

/// Testing-only seams inside [`install_impl_with_hook`]'s critical
/// section: a callback for each point a concurrent writer could land,
/// letting tests deterministically land a hostile write at a specific
/// window without any real threads or wall-clock races. Every field
/// defaults to `None` (a no-op); production code (via [`install_impl`])
/// always passes `TestHooks::default()`, so this is a no-op outside
/// tests.
#[derive(Default)]
struct TestHooks {
    /// Fires right after the initial read/fingerprint of an existing
    /// plist, right before the first TOCTOU re-check.
    before_first_check: Option<Box<dyn FnOnce()>>,
    /// Fires right after the first TOCTOU re-check passes, right before
    /// the (already-atomic) backup write.
    before_backup_write: Option<Box<dyn FnOnce()>>,
    /// Fires right after the backup write, right before the second
    /// TOCTOU re-check.
    before_second_check: Option<Box<dyn FnOnce()>>,
    /// Fires right after the second TOCTOU re-check passes, right before
    /// the final write that mutates the live plist.
    before_final_write: Option<Box<dyn FnOnce()>>,
}

/// The real body of [`install_impl`], parameterized on [`TestHooks`] so
/// tests can deterministically inject a hostile concurrent write at any
/// point in the critical section (see [`TestHooks`]'s field docs).
/// Production and [`install_impl`] itself always pass
/// `TestHooks::default()`; only tests populate individual fields. This is
/// the smallest addition that makes every abort window testable
/// end-to-end through `install_impl` — everything else about the
/// algorithm is unchanged.
fn install_impl_with_hook(
    plist_path: &Path,
    program_path: &Path,
    force: bool,
    log_dir: &Path,
    hooks: TestHooks,
) -> io::Result<InstallOutcome> {
    std::fs::create_dir_all(log_dir)?;

    if !plist_path.exists() {
        write_plist(
            plist_path,
            program_path,
            DEFAULT_START_INTERVAL_SECS,
            log_dir,
        )?;
        return Ok(InstallOutcome::Installed);
    }

    let existing_contents = std::fs::read_to_string(plist_path)?;
    let before_fingerprint = fingerprint(plist_path);

    // "Already up to date" means a completely fresh install with the
    // current inputs (default interval, current program path) would
    // produce exactly this file — i.e. there is nothing to preserve and
    // nothing to refresh. This is deliberately checked against
    // DEFAULT_START_INTERVAL_SECS rather than the file's own
    // (possibly hand-edited) interval: if the interval differs, that is
    // itself the "customization to preserve" the Repaired case exists
    // for, not a no-op.
    let effective_interval =
        extract_start_interval_secs(&existing_contents).unwrap_or(DEFAULT_START_INTERVAL_SECS);
    let fresh_default_candidate =
        generate_plist(program_path, DEFAULT_START_INTERVAL_SECS, log_dir);

    if existing_contents == fresh_default_candidate {
        return Ok(InstallOutcome::AlreadyUpToDate);
    }

    // The candidate that would actually be written if this turns out to
    // be a safe Repaired case: current program path, but the interval
    // preserved from whatever was already on disk.
    let candidate = generate_plist(program_path, effective_interval, log_dir);

    let existing_program_path = extract_program_path(&existing_contents);
    let matches_own_shape = existing_program_path
        .as_deref()
        .map(|p| generate_plist(Path::new(p), effective_interval, log_dir) == existing_contents)
        .unwrap_or(false);

    if !matches_own_shape && !force {
        return Ok(InstallOutcome::RefusedNeedsForce);
    }

    // Testing-only seam: let a test simulate a concurrent modification
    // landing between the initial read above and the first TOCTOU
    // re-check. Always `None` outside tests, so this is a no-op in
    // production.
    if let Some(hook) = hooks.before_first_check {
        hook();
    }

    // First TOCTOU guard: re-check right before mutating anything
    // (including the backup copy) so a call that aborts here truly
    // leaves zero mutation behind, matching
    // `AbortedConcurrentModification`'s contract.
    if file_changed_since(plist_path, before_fingerprint) {
        return Ok(InstallOutcome::AbortedConcurrentModification);
    }

    if let Some(hook) = hooks.before_backup_write {
        hook();
    }

    let backup_path = plist_path.with_extension("plist.bak");
    write_atomic(&backup_path, &existing_contents)?;

    if let Some(hook) = hooks.before_second_check {
        hook();
    }

    // Second TOCTOU guard: the first check above only covers the window
    // up to itself — a concurrent writer can still land between that
    // check and this point (or, without this second check, between here
    // and the final write below). Re-check against the *same*
    // `before_fingerprint` baseline (what `candidate` was actually
    // computed against, not a freshly recaptured one) so this catches
    // any drift since the original read, exactly like the first check.
    // If it fires, the live plist has already been left untouched, but
    // the backup write just above has happened: that backup contains the
    // pre-race `existing_contents`, never the concurrent writer's data,
    // so a stray-but-correct backup left behind here is not data loss —
    // it is not the "zero mutation" of the live plist this outcome
    // promises, but it is harmless and arguably useful (it's a copy of
    // what was on disk right before the abort). See
    // `book/src/daemon_lifecycle.md`.
    if file_changed_since(plist_path, before_fingerprint) {
        return Ok(InstallOutcome::AbortedConcurrentModification);
    }

    if let Some(hook) = hooks.before_final_write {
        hook();
    }

    write_atomic(plist_path, &candidate)?;

    let written = std::fs::read_to_string(plist_path)?;
    if written != candidate {
        return Err(io::Error::other(
            "plist read-back after write did not match the intended content",
        ));
    }

    Ok(InstallOutcome::Repaired)
}

/// Uninstalls the launch agent: asks `launchctl` to unload it (best-effort
/// — an already-unloaded agent reports an error from `launchctl` that we
/// don't treat as fatal here), backs up the plist if present, then removes
/// it. Idempotent: uninstalling an already-missing plist is not an error.
pub fn uninstall(plist_path: &Path) -> io::Result<()> {
    let _ = run_launchctl(&["unload", "-w", &plist_path.display().to_string()]);
    if plist_path.exists() {
        let contents = std::fs::read_to_string(plist_path)?;
        let backup_path = plist_path.with_extension("plist.bak");
        write_atomic(&backup_path, &contents)?;
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

    fn unique_temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-launchd-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn generate_plist_contains_label_and_program_path() {
        let log_dir = Path::new("/tmp/glomeris-logs-test");
        let xml = generate_plist(Path::new("/usr/local/bin/glomeris"), 42, log_dir);
        assert!(xml.contains(LABEL));
        assert!(xml.contains("/usr/local/bin/glomeris"));
        assert!(xml.contains("<integer>42</integer>"));
        assert!(xml.starts_with("<?xml"));
    }

    #[test]
    fn generate_plist_uses_log_dir_parameter_not_tmp() {
        let log_dir = Path::new("/Users/example/Library/Logs/Glomeris");
        let xml = generate_plist(Path::new("/usr/local/bin/glomeris"), 42, log_dir);
        assert!(xml.contains("/Users/example/Library/Logs/Glomeris/monitor.log"));
        assert!(xml.contains("/Users/example/Library/Logs/Glomeris/monitor.err.log"));
        assert!(!xml.contains("/tmp/glomeris-monitor"));
    }

    #[test]
    fn generate_plist_escapes_xml_special_characters_in_path() {
        let log_dir = Path::new("/tmp/glomeris-logs-test");
        let xml = generate_plist(Path::new("/tmp/a&b<c>\"d"), 1, log_dir);
        assert!(!xml.contains("a&b<c>\"d"));
        assert!(xml.contains("a&amp;b&lt;c&gt;&quot;d"));
    }

    #[test]
    fn write_plist_creates_parent_dirs_and_readable_file() {
        let dir = unique_temp_dir("write-plist");
        let plist_path = dir.join("nested").join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        write_plist(&plist_path, Path::new("/bin/echo"), 60, &log_dir)
            .expect("write_plist should succeed");
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
        let dir = unique_temp_dir("lifecycle");
        let plist_path = dir.join(format!("{LABEL}.plist"));

        let install_result = install(&plist_path, Path::new("/bin/echo"), false);
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

    #[test]
    fn default_log_dir_uses_home_and_glomeris_subdir() {
        let path = default_log_dir().expect("HOME should be set in test environment");
        assert!(path.to_string_lossy().contains("Library/Logs/Glomeris"));
    }

    #[test]
    fn first_install_writes_fresh_plist_with_default_interval() {
        let dir = unique_temp_dir("first-install");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        let outcome = install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir)
            .expect("install_impl should succeed");
        assert_eq!(outcome, InstallOutcome::Installed);
        let contents = std::fs::read_to_string(&plist_path).unwrap();
        assert!(contents.contains(&format!("<integer>{DEFAULT_START_INTERVAL_SECS}</integer>")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_install_with_no_changes_is_idempotent() {
        let dir = unique_temp_dir("no-changes");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let before = std::fs::read_to_string(&plist_path).unwrap();

        let outcome = install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        assert_eq!(outcome, InstallOutcome::AlreadyUpToDate);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert_eq!(before, after);
        assert!(!plist_path.with_extension("plist.bak").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_preserves_hand_edited_start_interval() {
        let dir = unique_temp_dir("preserve-interval");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let hand_edited = std::fs::read_to_string(&plist_path).unwrap().replace(
            &format!("<integer>{DEFAULT_START_INTERVAL_SECS}</integer>"),
            "<integer>120</integer>",
        );
        std::fs::write(&plist_path, &hand_edited).unwrap();

        let outcome = install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        assert_eq!(outcome, InstallOutcome::Repaired);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert!(after.contains("<integer>120</integer>"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_refreshes_program_path_without_force() {
        let dir = unique_temp_dir("refresh-path");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo-a"), false, &log_dir).unwrap();
        let outcome = install_impl(&plist_path, Path::new("/bin/echo-b"), false, &log_dir).unwrap();
        assert_eq!(outcome, InstallOutcome::Repaired);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert!(after.contains("/bin/echo-b"));
        assert!(!after.contains("/bin/echo-a"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_refuses_unknown_customization_without_force() {
        let dir = unique_temp_dir("refuse-unknown");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let hand_edited = std::fs::read_to_string(&plist_path)
            .unwrap()
            .replace("<true/>", "<false/>");
        std::fs::write(&plist_path, &hand_edited).unwrap();
        let before = std::fs::read_to_string(&plist_path).unwrap();

        let outcome = install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        assert_eq!(outcome, InstallOutcome::RefusedNeedsForce);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert_eq!(before, after, "refusal must leave the plist byte-identical");
        assert!(!plist_path.with_extension("plist.bak").exists());
        assert!(!plist_path.with_extension("plist.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_with_force_overwrites_unknown_customization_and_takes_backup() {
        let dir = unique_temp_dir("force-overwrite");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let hand_edited = std::fs::read_to_string(&plist_path)
            .unwrap()
            .replace("<true/>", "<false/>");
        std::fs::write(&plist_path, &hand_edited).unwrap();

        let outcome = install_impl(&plist_path, Path::new("/bin/echo"), true, &log_dir).unwrap();
        assert_eq!(outcome, InstallOutcome::Repaired);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert!(after.contains("<true/>"));

        let backup_path = plist_path.with_extension("plist.bak");
        assert!(backup_path.exists());
        let backup_contents = std::fs::read_to_string(&backup_path).unwrap();
        assert_eq!(backup_contents, hand_edited);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupted_write_never_produces_a_partial_plist() {
        // Proof 1: the success path never leaves a `.plist.tmp` sibling.
        let dir = unique_temp_dir("no-tmp-leftover");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");
        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        assert!(!plist_path.with_extension("plist.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);

        // Proof 2: `write_atomic` failing mid-write never disturbs an
        // existing file. We force the initial temp-file write to fail by
        // pointing it at a directory that was never created (so the
        // sibling temp-file write errors with "not found") — a real,
        // reproducible failure mode that doesn't require root or
        // permission tricks.
        let dir2 = unique_temp_dir("atomic-failure");
        // Deliberately NOT creating `dir2` — the parent directory of
        // `target_path` doesn't exist, so `write_atomic`'s temp-file write
        // fails before any rename is attempted.
        let target_path = dir2.join(format!("{LABEL}.plist"));
        let result = write_atomic(&target_path, "new content");
        assert!(result.is_err());
        assert!(
            !target_path.exists(),
            "a failed atomic write must not leave a partial target file"
        );
        assert!(!target_path.with_extension("plist.tmp").exists());
    }

    #[test]
    fn uninstall_backs_up_before_removing() {
        let dir = unique_temp_dir("uninstall-backup");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let original = std::fs::read_to_string(&plist_path).unwrap();

        uninstall(&plist_path).unwrap();
        assert!(!plist_path.exists());
        let backup_path = plist_path.with_extension("plist.bak");
        assert!(backup_path.exists());
        let backup_contents = std::fs::read_to_string(&backup_path).unwrap();
        assert_eq!(backup_contents, original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_uninstall_is_idempotent() {
        let dir = unique_temp_dir("double-uninstall");
        let plist_path = dir.join(format!("{LABEL}.plist"));

        assert!(uninstall(&plist_path).is_ok());
        assert!(uninstall(&plist_path).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_changed_since_detects_a_real_on_disk_change() {
        let dir = unique_temp_dir("fingerprint");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.txt");

        std::fs::write(&path, "short").unwrap();
        let before = fingerprint(&path);
        assert!(
            !file_changed_since(&path, before),
            "unchanged file must report unchanged"
        );

        // Use a different length (not just a re-write) so this assertion
        // can't flake on coarse filesystem mtime resolution — length
        // alone is enough to prove the change is detected.
        std::fs::write(&path, "a much longer replacement body").unwrap();
        assert!(
            file_changed_since(&path, before),
            "modified file must report changed even under coarse mtime granularity"
        );

        // Deletion counts as a change too.
        let before2 = fingerprint(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(file_changed_since(&path, before2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_start_interval_secs_refuses_ambiguous_duplicate_marker() {
        // A legitimate `<key>StartInterval</key>` plus decoy text elsewhere
        // in the document containing that same literal marker (e.g. an
        // adversarially/coincidentally crafted `ProgramArguments` string)
        // must not fool the extractor into picking either occurrence.
        let xml = "<plist><dict>\
            <key>ProgramArguments</key><array><string>decoy \
            <key>StartInterval</key><integer>999</integer></string></array>\
            <key>StartInterval</key><integer>60</integer>\
            </dict></plist>";
        assert_eq!(extract_start_interval_secs(xml), None);
    }

    #[test]
    fn extract_program_path_refuses_ambiguous_duplicate_marker() {
        let xml = "<plist><dict>\
            <key>Comment</key><string>decoy \
            <key>ProgramArguments</key><array><string>/decoy/path</string></array></string>\
            <key>ProgramArguments</key><array><string>/real/path</string></array>\
            </dict></plist>";
        assert_eq!(extract_program_path(xml), None);
    }

    #[test]
    fn uninstall_backup_write_failure_leaves_original_plist_intact() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("backup-atomic-failure");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");
        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let original = std::fs::read_to_string(&plist_path).unwrap();

        // Make the directory read-only so the backup's temp-file write
        // (which must create a new file) fails before any rename is
        // attempted — proving the backup goes through the same atomic
        // temp-file+rename mechanism as the main plist write rather than
        // a plain, non-atomic `fs::copy`.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = uninstall(&plist_path);

        // Restore write permission before any further filesystem access,
        // including test cleanup.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "uninstall must surface the backup write failure rather than swallow it"
        );
        assert!(
            plist_path.exists(),
            "original plist must survive a failed backup write"
        );
        assert_eq!(std::fs::read_to_string(&plist_path).unwrap(), original);
        assert!(!plist_path.with_extension("plist.bak").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_impl_hook_triggers_aborted_concurrent_modification() {
        let dir = unique_temp_dir("concurrent-abort");
        let plist_path = dir.join(format!("{LABEL}.plist"));
        let log_dir = dir.join("logs");

        // Seed an existing plist with a hand-edited StartInterval so this
        // call takes the "would repair" branch and actually reaches the
        // TOCTOU re-check below.
        install_impl(&plist_path, Path::new("/bin/echo"), false, &log_dir).unwrap();
        let hand_edited = std::fs::read_to_string(&plist_path).unwrap().replace(
            &format!("<integer>{DEFAULT_START_INTERVAL_SECS}</integer>"),
            "<integer>120</integer>",
        );
        std::fs::write(&plist_path, &hand_edited).unwrap();

        let plist_path_for_hook = plist_path.clone();
        let hook: Box<dyn FnOnce()> = Box::new(move || {
            // Simulate a concurrent writer landing between install_impl's
            // initial read/fingerprint and its final TOCTOU re-check.
            std::fs::write(
                &plist_path_for_hook,
                "concurrently modified by someone else",
            )
            .unwrap();
        });

        let outcome = install_impl_with_hook(
            &plist_path,
            Path::new("/bin/echo"),
            false,
            &log_dir,
            TestHooks {
                before_first_check: Some(hook),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(outcome, InstallOutcome::AbortedConcurrentModification);
        let after = std::fs::read_to_string(&plist_path).unwrap();
        assert_eq!(
            after, "concurrently modified by someone else",
            "the concurrent writer's content must be left untouched"
        );
        assert!(
            !plist_path.with_extension("plist.bak").exists(),
            "an aborted install must take no backup"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
