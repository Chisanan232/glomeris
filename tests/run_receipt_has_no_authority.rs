//! HORO-2362 guard: the run receipt and per-resource size history are
//! report-only.
//!
//! The receipt (`reporting::receipt`) and the sample store
//! (`monitor::resource_history`) turn stored byte and mtime samples into
//! "grew X since last run" and "idle N days". That is explanation for a person.
//! It must never reach `classify`, the planner, Autopilot, the emergency path or
//! an action: age alone is never a reason (ADR-0001 §3.7, §19), and a resource
//! that grew or sat idle has not become any more deletable.
//!
//! Like `size_measurement_has_no_authority.rs`, this reads production source
//! only (comments and unit-test modules are stripped), and it also fails if the
//! producers stop naming the symbols, so the absence checks cannot pass
//! vacuously.

use std::fs;
use std::path::Path;

const DECIDING_LAYERS: [&str; 8] = [
    "src/policy",
    "src/autopilot",
    "src/planner",
    "src/executor",
    "src/emergency",
    "src/actions",
    "src/evidence",
    "src/detectors",
];

/// Symbols that exist only on the reporting side. The executor may record plain
/// data into its report (`ExecutedStep`, `ObservedCandidate`), but none of these.
const REPORT_ONLY_NAMES: [&str; 10] = [
    "RunReceipt",
    "run_receipt",
    "reporting::receipt",
    "ResourceGrowth",
    "ResourceHistory",
    "resource_history",
    "idle_days",
    "samples_to_record",
    "build_run_receipt",
    "MAX_GROWTH_ENTRIES",
];

fn production_source(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Drop every whole `#[cfg(test)] mod ... { }` block, and keep what follows
    // it: several files hold production code after their test module. rustfmt
    // closes a top-level module with a lone `}` at column 0, and an inline
    // `#[cfg(test)]` on a single statement is production code around it.
    let mut kept = Vec::new();
    let mut in_test_mod = false;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if in_test_mod {
            if line == "}" {
                in_test_mod = false;
            }
            continue;
        }
        if line.trim() == "#[cfg(test)]"
            && lines.peek().is_some_and(|next| {
                let next = next.trim_start();
                next.starts_with("mod ") || next.starts_with("pub mod ")
            })
        {
            // A `mod name;` declaration has no body to skip.
            let next = lines.peek().unwrap();
            if !next.trim_end().ends_with(';') {
                in_test_mod = true;
            }
            continue;
        }
        if !line.trim_start().starts_with("//") {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_deciding_layer_names_receipt_or_size_history_symbols() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scanned = 0;
    for layer in DECIDING_LAYERS {
        let mut files = Vec::new();
        rust_files(&root.join(layer), &mut files);
        for file in files {
            scanned += 1;
            let src = production_source(&file);
            for name in REPORT_ONLY_NAMES {
                assert!(
                    !src.contains(name),
                    "{} must not name `{name}`: growth, age and the receipt are report-only (HORO-2362)",
                    file.strip_prefix(root).unwrap().display()
                );
            }
        }
    }
    assert!(
        scanned > 40,
        "scanned only {scanned} files; the layer list drifted"
    );
}

/// The recovery loop records what it did and saw into its report. It may only
/// write those fields; reading them back would let the run's own history steer
/// its next choice.
///
/// An allow-list rather than a list of forbidden reads: every production line
/// that names the recorded fields (or the `log` that carries them) must be one
/// of the statements below, so a `for x in &log.observed_candidates`, an index,
/// or a multi-line `.iter()` fails here like any other read. Each recording
/// statement must also still exist, or the guard would pass with nothing recorded.
#[test]
fn the_recovery_loop_records_run_data_but_never_reads_it_back() {
    const FIELDS: [&str; 5] = [
        "executed_steps",
        "observed_candidates",
        "volume_readings_complete",
        "volume_reading_failed",
        "first_pass_taken",
    ];
    // Trimmed line prefixes: declarations, zero-initialisation, hand-over to the
    // report, and the writes themselves.
    const ALLOWED: [&str; 16] = [
        "pub executed_steps: Vec<ExecutedStep>,",
        "pub observed_candidates: Vec<ObservedCandidate>,",
        "pub volume_readings_complete: bool,",
        "executed_steps: Vec<ExecutedStep>,",
        "observed_candidates: Vec<ObservedCandidate>,",
        "volume_reading_failed: bool,",
        "first_pass_taken: bool,",
        "executed_steps: Vec::new(),",
        "observed_candidates: Vec::new(),",
        "volume_readings_complete: true,",
        "report.executed_steps = log.executed_steps;",
        "report.observed_candidates = log.observed_candidates;",
        "report.volume_readings_complete = !log.volume_reading_failed;",
        "log.volume_reading_failed = true;",
        "log.observed_candidates = candidates",
        "log.executed_steps.push(ExecutedStep {",
    ];
    // The one check-then-set of the once-per-run flag.
    const FIRST_PASS: [&str; 2] = ["if !log.first_pass_taken {", "log.first_pass_taken = true;"];

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = production_source(&root.join("src/executor/recovery_loop.rs"));
    let mut seen_allowed = std::collections::BTreeSet::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if !FIELDS.iter().any(|f| trimmed.contains(f)) {
            continue;
        }
        if FIRST_PASS.contains(&trimmed) {
            continue;
        }
        let allowed = ALLOWED.iter().find(|a| trimmed.starts_with(**a));
        assert!(
            allowed.is_some(),
            "recovery_loop.rs touches recorded run data outside the allowed writes: `{trimmed}`"
        );
        seen_allowed.insert(*allowed.unwrap());
    }
    for must in [
        "log.executed_steps.push(ExecutedStep {",
        "log.observed_candidates = candidates",
        "report.executed_steps = log.executed_steps;",
        "report.observed_candidates = log.observed_candidates;",
    ] {
        assert!(
            seen_allowed.contains(must),
            "the loop no longer records `{must}`; the guard would be vacuous"
        );
    }
}

#[test]
fn the_report_side_does_not_depend_on_policy_or_planning() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "src/reporting/receipt.rs",
        "src/monitor/resource_history.rs",
    ] {
        let src = production_source(&root.join(file));
        for forbidden in [
            "crate::policy",
            "crate::planner",
            "crate::autopilot",
            "classify",
        ] {
            assert!(
                !src.contains(forbidden),
                "{file} must not depend on `{forbidden}`: it explains, it does not decide"
            );
        }
    }
}

#[test]
fn the_producers_and_the_free_command_still_name_the_symbols() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let receipt = production_source(&root.join("src/reporting/receipt.rs"));
    assert!(receipt.contains("pub fn build_run_receipt"));
    assert!(receipt.contains("idle_days"));
    let store = production_source(&root.join("src/monitor/resource_history.rs"));
    assert!(store.contains("pub fn record"));
    let main = production_source(&root.join("src/main.rs"));
    assert!(main.contains("build_run_receipt"));
    assert!(main.contains("resource_history"));
}

#[test]
fn measured_and_estimated_figures_are_separate_fields_never_summed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let receipt = production_source(&root.join("src/reporting/receipt.rs"));
    // The only arithmetic on the measured side is free-after minus free-before.
    assert!(receipt.contains("report.final_free_bytes"));
    assert!(receipt.contains("report.started_free_bytes"));
    for line in receipt.lines() {
        let mixes = line.contains("total_bytes_freed")
            || (line.contains("volume_free_delta") && line.contains("estimated_bytes"));
        assert!(
            !mixes,
            "receipt.rs mixes measured and estimated figures: `{}`",
            line.trim()
        );
    }
}
