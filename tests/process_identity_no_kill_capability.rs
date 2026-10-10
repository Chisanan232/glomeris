//! HORO-1823 non-negotiable: process-liveness evidence adds NO signal/kill
//! authority, anywhere, not even behind a feature flag. This is a
//! structural proof over source text, not a runtime behavior test — the
//! whole point is that the capability must not exist at all, so there is
//! no runtime scenario to exercise.
//!
//! Modelled on the repo's other `check-*`-style structural guards
//! (`scripts/check-host-dependency-probe-is-read-only.sh`), but kept as a
//! `cargo test` rather than a shell script since the property is narrow
//! enough to express directly.

use std::fs;
use std::path::Path;

const NEW_MODULE: &str = "src/evidence/correlate/process_identity.rs";

/// `process_identity.rs` may spawn `ps`, `launchctl` and `lsof` — all
/// read-only queries — and may reap ITS OWN spawned child via
/// `run_with_timeout` (shared with every other probe in this crate). It
/// must never call anything that sends a signal to another process: no
/// `kill`, `SIGTERM`, `SIGKILL`, `libc::kill`, `nix::sys::signal`, no
/// `Command::new` naming `kill`/`pkill`/`killall`.
#[test]
fn process_identity_module_names_no_signal_or_kill_primitive() {
    let text = fs::read_to_string(NEW_MODULE).expect("process_identity.rs must exist");
    let forbidden = [
        "libc::kill",
        "nix::sys::signal",
        "SIGTERM",
        "SIGKILL",
        "SIGINT",
        "\"kill\"",
        "\"pkill\"",
        "\"killall\"",
        ".signal(",
    ];
    for needle in forbidden {
        assert!(
            !text.contains(needle),
            "process_identity.rs must never reference {needle:?} — HORO-1823 grants no kill/signal authority"
        );
    }
}

/// The only subprocess binaries this module names are the three
/// read-only tools the ADR authorizes (`ps`, `launchctl`, `lsof`) plus
/// `date` (used only to convert `ps -o lstart=`'s text into a
/// `SystemTime` — never to mutate anything).
#[test]
fn process_identity_module_spawns_only_the_authorized_readonly_tools() {
    let text = fs::read_to_string(NEW_MODULE).expect("process_identity.rs must exist");
    for line in text.lines() {
        if let Some(idx) = line.find("Command::new(") {
            let rest = &line[idx + "Command::new(".len()..];
            // Every call site in this module passes an injected `&Path`
            // field (`ps_bin`, `launchctl_bin`, `lsof_bin`, `date_bin`),
            // never a literal string naming some other binary.
            let allowed = ["ps_bin", "launchctl_bin", "lsof_bin", "date_bin"];
            assert!(
                allowed.iter().any(|a| rest.contains(a)),
                "unexpected Command::new(...) target in process_identity.rs: {line}"
            );
        }
    }
}

/// `ActionStep` (`src/actions/mod.rs`) is the sole typed vocabulary of
/// what Glomeris is capable of executing. HORO-1823 must not add a
/// variant to it — process-liveness evidence is read-only observation,
/// never a new executable capability.
#[test]
fn action_step_still_has_exactly_two_variants() {
    let text = fs::read_to_string("src/actions/mod.rs").expect("src/actions/mod.rs must exist");
    let start = text
        .find("pub enum ActionStep")
        .expect("ActionStep enum must exist");
    let body_start = text[start..].find('{').unwrap() + start;
    let body_end = text[body_start..].find("\n}").unwrap() + body_start;
    let body = &text[body_start..body_end];
    assert_eq!(
        body.matches("RunTool {").count(),
        1,
        "RunTool variant must still be present exactly once"
    );
    assert_eq!(
        body.matches("DeletePath {").count(),
        1,
        "DeletePath variant must still be present exactly once"
    );
    // No third variant name has been introduced: every non-comment,
    // non-blank line at the enum's own (4-space) indent must start one of
    // the two known variants.
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//") {
            continue;
        }
        // Only check lines at the variant's own indentation (4 spaces) —
        // deeper-indented lines are a variant's own field declarations.
        if !line.starts_with("    ") || line.starts_with("     ") {
            continue;
        }
        assert!(
            trimmed.starts_with("RunTool")
                || trimmed.starts_with("DeletePath")
                || trimmed == "}"
                || trimmed == "},",
            "unexpected top-level line in ActionStep body (possible new variant): {line:?}"
        );
    }
}

/// `src/evidence/correlate/process_identity.rs`'s own doc comment and
/// production code must not claim or perform any write to the
/// filesystem (fixture setup in `#[cfg(test)]` is exempt) — this module
/// is read-only observation, matching the convention already enforced
/// for `host_dependency.rs`.
#[test]
fn process_identity_module_has_no_production_filesystem_write() {
    let text = fs::read_to_string(NEW_MODULE).expect("process_identity.rs must exist");
    let test_mod_start = text.find("#[cfg(test)]").unwrap_or(text.len());
    let production = &text[..test_mod_start];
    for needle in ["fs::write(", "fs::remove", "fs::rename(", "File::create("] {
        assert!(
            !production.contains(needle),
            "process_identity.rs production code must never write to the filesystem (found {needle:?})"
        );
    }
    let _ = Path::new(NEW_MODULE); // keep import used across edits
}
