//! The one rule for putting a used-percentage in front of a human (HORO-1506).
//!
//! ============================================================================
//! WHY A SHARED RULE IS NEEDED AT ALL
//! ============================================================================
//! Three different numbers in this product are all expressed as *percent used*:
//! where the disk is now, the alert threshold the user configured, and the
//! recovery goal. Two of them are compared against each other on every poll —
//! [`crate::monitor::PressureEpisodes::observe`] opens an episode when the
//! measurement reaches the threshold — and the third is the figure the user
//! reads on screen.
//!
//! Before this module the measurement was rendered with `{:.1}`, which rounds to
//! nearest, while every decision compared the unrounded `f64`. Those two rules
//! disagree in a band just under each tenth, and the disagreement is visible:
//!
//! ```text
//! measured used_percent      = 89.96
//! displayed (round to 1 dp)  = "90.0% used"
//! alert threshold            = 90.0% used
//! decision                   = 89.96 >= 90.0  ->  false  ->  no notification
//! ```
//!
//! The card states the threshold figure and nothing happens, which is the one
//! reading a storage tool cannot afford: a user who is told they are at their
//! alert threshold and not alerted has no way to tell a quiet product from a
//! broken one.
//!
//! ============================================================================
//! THE RULE, AND WHY IT ROUNDS DOWN
//! ============================================================================
//! A displayed used-percentage is the measurement **truncated** to one decimal
//! place — 89.96 reads as `"89.9% used"`, never `"90.0% used"`.
//!
//! The alternative was to round the *comparison* instead, so that 89.96 counts
//! as having reached 90. That was rejected: it fires a "tell me at 90%" alert at
//! 89.95%, and it would have changed pressure classification, the notification
//! threshold, the recovery-goal validity check and the closed loop's stopping
//! condition — four decisions that are currently correct. Nothing in this module
//! is reachable from any of them. [`crate::monitor::fs_stat::FsUsage::used_percent`]
//! remains the exact measurement and every comparison still uses it.
//!
//! Truncating instead makes the safe direction structural rather than
//! conventional:
//!
//! > if the displayed figure reads as at-or-past a threshold, the measurement
//! > really is at-or-past it.
//!
//! because `displayed(m) <= m`, so `displayed(m) >= t` implies `m >= t`. For a
//! whole-number threshold — which is all the GUI offers — the implication runs
//! both ways, so the screen and the decision cannot disagree at all.
//! `displayed_reaching_a_whole_threshold_means_the_decision_crossed_it` below is
//! that statement as a test.
//!
//! ============================================================================
//! WHAT THIS IS NOT
//! ============================================================================
//! Not a decision, and not a second source of truth for usage. It maps one
//! `f64` to one string. It reads no state, does no I/O, and is not consulted by
//! `monitor`, `policy`, `executor` or `planner`.
//!
//! The macOS app needs the same strings — it renders the same figure from the
//! same DTO field — so `GlomerisUsedPercent` ports this rule, and
//! `tests/fixtures/used_percent_golden.tsv` is the contract between them,
//! exactly as `human_bytes_golden.tsv` is for byte counts.

/// What a used-percentage reads as when the measurement is not a finite number.
///
/// A `NaN` is not 0% used, and saying so would be a fabricated reading of a
/// disk nobody measured. Rust's own `used_percent()` cannot produce one — it
/// returns 0.0 for a zero-capacity volume — but this value arrives in the macOS
/// app as JSON, where it can be anything, so both sides need an honest word for
/// it rather than a number.
pub const UNAVAILABLE_TEXT: &str = "usage unavailable";

/// The measurement truncated to one decimal place, or `None` when it is not a
/// finite number.
///
/// Clamped to `0.0..=100.0`. The clamp is defensive rather than expected:
/// `FsUsage::used_percent` saturates, so a reading outside the range means the
/// number did not come from a filesystem observation.
///
/// Implemented as `(m * 10.0).floor() / 10.0`. For every value with an exact
/// one-decimal decimal form in this range the multiplication lands on the
/// integer exactly — the rounding error of `m` is scaled by ten and is still
/// three orders of magnitude inside the gap between neighbouring `f64`s near
/// 1000 — so `displayed_used_percent(89.9)` is `89.9` and not `89.8`. For an
/// arbitrary measurement (a ratio of two byte counts, which is what this always
/// is in practice) a last-bit effect could move the result by around `1e-13`
/// percentage points; no threshold in this product is expressed to anywhere
/// near that precision.
pub fn displayed_used_percent(measured: f64) -> Option<f64> {
    if !measured.is_finite() {
        return None;
    }
    let clamped = measured.clamp(0.0, 100.0);
    Some((clamped * 10.0).floor() / 10.0)
}

/// `"89.9%"` — the figure alone, for a line that already says what axis it is
/// on (`glomeris status`' `used:` column).
pub fn used_percent_figure(measured: f64) -> String {
    match displayed_used_percent(measured) {
        Some(value) => format!("{value:.1}%"),
        None => UNAVAILABLE_TEXT.to_string(),
    }
}

/// `"89.9% used"` — the figure with its axis in it, for anywhere the axis is
/// not already established by a neighbouring label.
///
/// The axis word is part of the string on purpose. `"89.9%"` on its own has
/// been read as free space by more than one person, and the threshold and goal
/// settings both spell out `% used`, so the measurement they are compared
/// against says it too.
pub fn used_percent_text(measured: f64) -> String {
    match displayed_used_percent(measured) {
        Some(value) => format!("{value:.1}% used"),
        None => UNAVAILABLE_TEXT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_rather_than_rounding_to_nearest() {
        // The whole point of the module. `{:.1}` renders this as "90.0% used".
        assert_eq!(used_percent_text(89.96), "89.9% used");
        assert_eq!(used_percent_text(89.99999), "89.9% used");
    }

    #[test]
    fn a_value_already_at_one_decimal_place_survives_intact() {
        // Guards the floating-point trap in the implementation: if `89.9 * 10.0`
        // landed on 898.999… this would read "89.8% used" and every figure in
        // the product would be a tenth low.
        for tenths in 0..=1000u32 {
            let value = f64::from(tenths) / 10.0;
            assert_eq!(
                used_percent_text(value),
                format!("{value:.1}% used"),
                "a one-decimal value must render as itself: {value}"
            );
        }
    }

    #[test]
    fn zero_and_one_hundred_are_ordinary_readings() {
        assert_eq!(used_percent_text(0.0), "0.0% used");
        assert_eq!(used_percent_text(100.0), "100.0% used");
        assert_eq!(used_percent_figure(0.0), "0.0%");
        assert_eq!(used_percent_figure(100.0), "100.0%");
    }

    #[test]
    fn out_of_range_readings_are_clamped_not_rendered_raw() {
        assert_eq!(used_percent_text(-0.5), "0.0% used");
        assert_eq!(used_percent_text(140.0), "100.0% used");
    }

    #[test]
    fn a_non_finite_reading_is_named_rather_than_turned_into_zero() {
        for bogus in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(displayed_used_percent(bogus), None);
            assert_eq!(used_percent_text(bogus), UNAVAILABLE_TEXT);
            assert_eq!(used_percent_figure(bogus), UNAVAILABLE_TEXT);
        }
    }

    #[test]
    fn the_axis_word_is_in_the_sentence() {
        assert!(used_percent_text(42.0).ends_with("% used"));
    }

    /// The invariant this module exists to provide, stated over the whole range
    /// of thresholds the GUI can produce.
    ///
    /// `Threshold::is_crossed` and `EpisodeConfig`'s open boundary both compare
    /// the raw measurement with `>=`. This asserts the two directions that make
    /// the screen and that comparison the same statement for a whole-number
    /// threshold: a displayed figure that has reached the threshold means the
    /// measurement had, and a measurement that reached it always displays as
    /// having done so.
    #[test]
    fn displayed_reaching_a_whole_threshold_means_the_decision_crossed_it() {
        for threshold in 1..=99u32 {
            let t = f64::from(threshold);
            // Six readings straddling the boundary, including the two that the
            // old `{:.1}` rendering got wrong.
            for delta in [-0.04, -0.01, -0.000_1, 0.0, 0.000_1, 0.04] {
                let measured = t + delta;
                let displayed = displayed_used_percent(measured).expect("finite");
                let decision_crossed = measured >= t;
                assert_eq!(
                    displayed >= t,
                    decision_crossed,
                    "threshold {t}, measured {measured}: displayed {displayed} \
                     says crossed={}, the decision says crossed={decision_crossed}",
                    displayed >= t
                );
            }
        }
    }

    /// Never above the measurement, for any reading. This is the half of the
    /// invariant that holds for a fractional threshold too — the CLI accepts
    /// one even though the GUI stepper does not offer it — so the display can
    /// still never overstate a crossing.
    #[test]
    fn a_displayed_figure_never_exceeds_the_measurement() {
        for hundredths in 0..=10_000u32 {
            let measured = f64::from(hundredths) / 100.0;
            let displayed = displayed_used_percent(measured).expect("finite");
            assert!(
                displayed <= measured + 1e-9,
                "{displayed} must not exceed the measured {measured}"
            );
        }
    }

    /// HORO-1506. The macOS app renders this same figure from the same DTO
    /// field, so the rule is implemented twice and pinned once. This test makes
    /// the fixture Rust's output by definition; `GlomerisUsedPercentTests`
    /// asserts the same file. Either side moving fails in the language that
    /// moved, and changing the convention on purpose means editing the fixture
    /// and then following in both languages.
    ///
    /// Modelled on `bytes::golden_fixture_pins_the_shared_convention`, which
    /// established this arrangement for byte counts in HORO-1452.
    #[test]
    fn golden_fixture_pins_the_shared_convention() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/used_percent_golden.tsv"
        );
        let text = std::fs::read_to_string(path).expect("golden fixture must be readable");

        let mut checked = 0usize;
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (measured, figure, sentence) = (
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            );
            assert!(
                parts.next().is_none(),
                "fixture line has more than three columns: {line}"
            );
            assert!(
                !figure.is_empty() && !sentence.is_empty(),
                "fixture line is not <measured>\\t<figure>\\t<sentence>: {line}"
            );
            let value: f64 = measured
                .parse()
                .unwrap_or_else(|e| panic!("fixture measurement {measured} does not parse: {e}"));
            assert_eq!(used_percent_figure(value), figure, "figure for {measured}");
            assert_eq!(
                used_percent_text(value),
                sentence,
                "sentence for {measured}"
            );
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
