//! HORO-2360 guard: on-disk size measurements are report-only.
//!
//! `Evidence::physical_bytes` (allocated bytes from `st_blocks`) may be
//! produced by the size walk and shown by `explain`. It must never be read by
//! anything that decides, plans, executes or talks to a model: ADR-0001 §19.
//! This test reads production source only (comment lines and the unit-test
//! module are stripped) and fails if a deciding layer names the figure. It
//! also fails if the producer or the report surface stops naming it, so the
//! absence checks cannot pass vacuously.

use std::fs;
use std::path::Path;

const DECIDING_LAYERS: [&str; 6] = [
    "src/policy",
    "src/autopilot",
    "src/planner",
    "src/executor",
    "src/emergency",
    "src/actions",
];

const MEASUREMENT_NAMES: [&str; 2] = ["physical_bytes", "allocated_bytes"];

fn production_source(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Only a whole test module ends production code: an inline
    // `#[cfg(test)]` on a single statement must not hide what follows it.
    let code = match text.find("#[cfg(test)]\nmod ") {
        Some(at) => &text[..at],
        None => &text,
    };
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
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
fn no_deciding_layer_reads_an_on_disk_size_measurement() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scanned = 0;
    for layer in DECIDING_LAYERS {
        let mut files = Vec::new();
        rust_files(&root.join(layer), &mut files);
        for file in files {
            scanned += 1;
            let src = production_source(&file);
            for name in MEASUREMENT_NAMES {
                assert!(
                    !src.contains(name),
                    "{} must not name `{name}`: on-disk size is report-only (ADR-0001 §19)",
                    file.strip_prefix(root).unwrap().display()
                );
            }
        }
    }
    assert!(
        scanned > 20,
        "scanned only {scanned} files; the layer list drifted"
    );
}

#[test]
fn the_producer_and_the_report_surface_still_name_the_measurement() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let detectors = production_source(&root.join("src/detectors/mod.rs"));
    assert!(detectors.contains("allocated_bytes_of"));
    assert!(detectors.contains("st_blocks") || detectors.contains("blocks()"));
    let dto = production_source(&root.join("src/reporting/dto.rs"));
    assert!(dto.contains("physical_bytes"));
    assert!(dto.contains("recovery_bound"));
}
