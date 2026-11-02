//! HORO-2359 guard: the Config marker probe is evidence only.
//!
//! The probe in `src/evidence/protective_marker.rs` may only look (list a
//! directory, `lstat` a name). This test reads its production source and fails
//! on anything that could mutate, read content, or grant authority, and on any
//! surface that would hand the marker data to a model. It also fails if the
//! probe stops looking for the markers at all, so the absence checks cannot
//! pass vacuously.

fn production_source(file: &str) -> String {
    let full = format!("{}/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("read {full}: {e}"));
    // Everything before the unit-test module is production code, and comment
    // lines are not code: the module docs legitimately name what it avoids.
    let code = match text.find("#[cfg(test)]") {
        Some(at) => &text[..at],
        None => &text,
    };
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_probe_only_looks() {
    let src = production_source("src/evidence/protective_marker.rs");
    for forbidden in [
        "fs::write",
        "fs::remove",
        "fs::rename",
        "fs::create",
        "fs::copy",
        "fs::set_permissions",
        "OpenOptions",
        "File::open",
        "File::create",
        "fs::read(",
        "read_to_string",
        "serde",
        "Command::new",
        "AutoSafe",
        "unsafe",
    ] {
        assert!(
            !src.contains(forbidden),
            "the Config marker probe must not use `{forbidden}`"
        );
    }
}

#[test]
fn the_probe_never_names_a_receipt_field() {
    let src = production_source("src/evidence/protective_marker.rs");
    for field in ["reproducible", "evidence_free", "session_id", "\"private\""] {
        assert!(
            !src.contains(field),
            "a receipt field `{field}` must never be read by the probe"
        );
    }
}

#[test]
fn the_probe_still_looks_for_both_markers_without_following_links() {
    let src = production_source("src/evidence/protective_marker.rs");
    assert!(src.contains(".ca-keep"));
    assert!(src.contains(".ca-owner.json"));
    assert!(src.contains("symlink_metadata"));
    assert!(src.contains("read_dir"));
}

#[test]
fn marker_data_never_reaches_a_model_or_a_report_dto() {
    for file in [
        "src/planner/dto.rs",
        "src/planner/project.rs",
        "src/actions/llm.rs",
        "src/reporting/dto.rs",
    ] {
        let path = format!("{}/{file}", env!("CARGO_MANIFEST_DIR"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        assert!(
            !text.contains("config_markers"),
            "{file} must not carry Config marker data"
        );
    }
}
