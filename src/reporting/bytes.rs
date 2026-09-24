//! Human-readable byte formatting for CLI reports (HORO-955).
//!
//! Pure, dependency-free: no I/O, no ambient state. Used everywhere a
//! report needs to render a `u64` byte count for a human (`glomeris
//! status`/`detect`/`explain`/`clean --dry-run`).

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Formats `bytes` as a human-readable string using binary (1024-based)
/// units, e.g. `"1.2 GB"`, `"340.5 MB"`. Counts under 1024 bytes are
/// rendered as a bare integer (`"512 B"`, `"0 B"`) — a fractional digit on
/// a whole-byte count would be noise, not precision.
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_idx = 0usize;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    format!("{value:.1} {}", UNITS[unit_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_bytes_is_bare_zero() {
        assert_eq!(human_bytes(0), "0 B");
    }

    #[test]
    fn below_1024_is_bare_byte_count() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
    }

    #[test]
    fn exactly_1024_is_one_point_zero_kb() {
        assert_eq!(human_bytes(1024), "1.0 KB");
    }

    #[test]
    fn megabytes_round_to_one_decimal() {
        assert_eq!(human_bytes(142_300_000), "135.7 MB");
    }

    #[test]
    fn gigabytes_round_to_one_decimal() {
        assert_eq!(human_bytes(1_288_490_189), "1.2 GB");
    }

    #[test]
    fn terabyte_boundary() {
        assert_eq!(human_bytes(1024u64.pow(4)), "1.0 TB");
    }

    #[test]
    fn very_large_value_does_not_panic_and_caps_at_petabytes() {
        let s = human_bytes(u64::MAX);
        assert!(s.ends_with(" PB"), "expected a PB-suffixed value, got {s}");
    }

    #[test]
    fn just_under_next_unit_boundary_stays_in_lower_unit() {
        // 1024*1024 - 1 bytes: still under the MB boundary, so KB.
        assert_eq!(human_bytes(1024 * 1024 - 1), "1024.0 KB");
    }

    /// HORO-1452. This function is no longer the only implementation of the
    /// convention: the macOS app has to render two figures no CLI invocation
    /// ever produces — the Apply Plan batch estimate and the batch reclaimed
    /// total, both sums over N single-item `execute` calls with no batch report
    /// to carry a rendered string — so `GlomerisByteFormat.human` ports the
    /// same rule.
    ///
    /// The fixture is the contract between them. This test makes it Rust's
    /// output by definition; the Swift suite asserts the same file. Neither
    /// side can drift without a failure, and changing the convention on purpose
    /// means editing the fixture and then following in both languages.
    #[test]
    fn golden_fixture_pins_the_shared_convention() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/human_bytes_golden.tsv"
        );
        let text = std::fs::read_to_string(path).expect("golden fixture must be readable");

        let mut checked = 0usize;
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (bytes, expected) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("fixture line is not <bytes>\\t<rendered>: {line}"));
            let bytes: u64 = bytes
                .parse()
                .unwrap_or_else(|e| panic!("fixture byte count {bytes} does not parse: {e}"));
            assert_eq!(human_bytes(bytes), expected, "for {bytes} bytes");
            checked += 1;
        }

        // A fixture that failed to parse, or that someone emptied, would
        // otherwise pass this test by asserting nothing at all.
        assert!(
            checked >= 16,
            "expected the fixture to still cover at least 16 values, checked {checked}"
        );
    }
}
