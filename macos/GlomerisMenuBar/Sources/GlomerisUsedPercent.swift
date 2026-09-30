//
//  GlomerisUsedPercent.swift
//  GlomerisMenuBar
//
//  HORO-1506: the one place this app renders a measured used-percentage.
//
//  ============================================================================
//  WHY A SHARED RULE, AND WHY IT ROUNDS DOWN
//  ============================================================================
//  Three figures in this product sit on the same axis — where the disk is now,
//  the alert threshold the user configured, and the recovery goal — and two of
//  them are compared against each other on every poll. The comparison is the
//  CLI's, against the unrounded `f64`; the figure on screen was
//  `String(format: "%.1f%% used", …)`, which rounds to nearest. Those two rules
//  disagree in a band just under each tenth, and the disagreement is visible:
//
//      measured used_percent      = 89.96
//      displayed (round to 1 dp)  = "90.0% used"
//      alert threshold            = 90.0% used
//      decision                   = 89.96 >= 90.0  ->  no notification
//
//  The card states the threshold figure and nothing happens, which is the one
//  reading a storage tool cannot afford.
//
//  The rule is therefore to *truncate* to one decimal place — 89.96 reads as
//  "89.9% used". The alternative, rounding the comparison instead, would have
//  fired a "tell me at 90%" alert at 89.95% and would have changed four
//  decisions that are currently correct. Truncating changes none of them and
//  makes the safe direction structural:
//
//      if the displayed figure reads as at-or-past a threshold, the
//      measurement really is at-or-past it
//
//  because `displayed(m) <= m`. For a whole-number threshold — all the settings
//  stepper offers — the implication runs both ways, so the screen and the
//  decision cannot disagree at all.
//
//  ============================================================================
//  THE CONTRACT WITH RUST
//  ============================================================================
//  Rust's `reporting::used_percent` implements the same rule for the CLI's own
//  output, and `tests/fixtures/used_percent_golden.tsv` pins the two to each
//  other: Rust's `golden_fixture_pins_the_shared_convention` makes the file
//  Rust's output by definition, and `GlomerisUsedPercentTests` holds this port
//  to the same file. Either side moving fails in the language that moved. This
//  is the arrangement `GlomerisByteFormat` established for byte counts in
//  HORO-1452, for the same reason.
//
//  ============================================================================
//  WHAT THIS IS NOT
//  ============================================================================
//  Not a decision, and not a second source of truth for usage. It maps one
//  `Double` to one string, imports Foundation only, reads no state and cannot
//  reach the CLI. This app still never compares a percentage against a
//  threshold to decide anything — `PressureEpisodeMonitor` reads
//  `notification_due` — and nothing here changes that.
//
//  It is also not the renderer for a *configured* figure. A threshold or goal
//  the user chose is a whole number they typed, and `RecoverySettingsFigure`
//  renders it as one ("85% used", not "85.0% used"). This is only for a
//  measurement, which always has decimals it did not choose.
//

import Foundation

/// Renders a measured used-percentage the way `reporting::used_percent` does:
/// truncated to one decimal place, clamped to `0...100`, and named rather than
/// zeroed when it is not a finite number.
enum GlomerisUsedPercent {
    /// What a used-percentage reads as when the measurement is not a finite
    /// number.
    ///
    /// `NaN` is not 0% used, and saying so would be a fabricated reading of a
    /// disk nobody measured. The CLI's own `used_percent()` cannot produce one,
    /// but it arrives here as JSON, where it can be anything.
    static let unavailableText = "usage unavailable"

    /// The measurement truncated to one decimal place, or `nil` when it is not
    /// finite.
    ///
    /// `(m * 10).rounded(.down) / 10` — `.down` and not `.towardZero`, because
    /// the clamp has already removed the negative side and the two agree above
    /// zero, but `.down` is the rule the name states. For every value with an
    /// exact one-decimal form in this range the multiplication lands on the
    /// integer exactly, so `displayed(89.9)` is `89.9` and not `89.8`; the
    /// Swift suite sweeps all 1001 of them, as Rust's does.
    static func displayed(_ measured: Double) -> Double? {
        guard measured.isFinite else { return nil }
        let clamped = min(max(measured, 0), 100)
        return (clamped * 10).rounded(.down) / 10
    }

    /// `"89.9%"` — the figure alone, for a line that already says what axis it
    /// is on.
    static func figure(_ measured: Double) -> String {
        guard let value = displayed(measured) else { return unavailableText }
        return oneDecimalPlace(value) + "%"
    }

    /// `"89.9% used"` — the figure with its axis in it, for anywhere the axis
    /// is not already established by a neighbouring label.
    static func text(_ measured: Double) -> String {
        guard let value = displayed(measured) else { return unavailableText }
        return oneDecimalPlace(value) + "% used"
    }

    /// `"89.9 percent used"` — what VoiceOver reads.
    ///
    /// The axis word is not optional here. A bare "89.9 percent" has been heard
    /// as free space, and it is the *only* thing a screen-reader user gets from
    /// a progress bar, where a sighted user has the badge and the byte figures
    /// beside it. `RecoverySettingsFigure.spokenValue` says "percent used" for
    /// the configured figures for the same reason, so the measurement and the
    /// threshold are spoken on one axis.
    ///
    /// "%" is spelled out rather than left as a symbol because a spoken string
    /// is read, not parsed, and VoiceOver's handling of a bare "%" depends on
    /// the surrounding text.
    static func spoken(_ measured: Double) -> String {
        guard let value = displayed(measured) else { return unavailableText }
        return oneDecimalPlace(value) + " percent used"
    }

    /// One decimal place, with the locale pinned.
    ///
    /// `locale: nil` is load-bearing, not boilerplate — Rust's `{:.1}` always
    /// writes a full stop, and a localised formatter would write a comma under
    /// a European locale, so the two languages would disagree for a reason no
    /// en_US CI runner would surface. `GlomerisByteFormat` pins its locale for
    /// exactly the same reason.
    ///
    /// Rounding cannot bite here: `displayed` has already truncated to a value
    /// representable at one decimal place, so `%.1f` has nothing left to round.
    private static func oneDecimalPlace(_ displayedValue: Double) -> String {
        String(format: "%.1f", locale: nil, displayedValue)
    }
}
