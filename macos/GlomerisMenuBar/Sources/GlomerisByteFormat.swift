//
//  GlomerisByteFormat.swift
//  GlomerisMenuBar
//
//  HORO-1452: the one place this app renders a byte count it was not given a
//  rendering for.
//
//  ============================================================================
//  WHY THIS EXISTS AT ALL, GIVEN HORO-1312
//  ============================================================================
//  The standing rule in this target is that Rust owns the convention and this
//  app quotes the string Rust rendered. That rule stands, and every size that
//  arrives with a `*_human` key still goes out unchanged — see
//  `humanByteCount(_:rendered:)`, which prefers the CLI's string and only falls
//  through to here when there isn't one.
//
//  Two figures in Apply Plan have no CLI string to quote, and cannot get one:
//  the batch estimate and the batch reclaimed total. A batch is N single-item
//  `execute` invocations by design, so there is no batch report for Rust to
//  render a sum into. Before this ticket those two lines printed the raw count
//  — "About 5242880 bytes would be reclaimed." — in the middle of a
//  confirmation surface where every neighbouring figure was humanised.
//
//  HORO-1312's reason for the raw fallback was never "raw is good". It was that
//  a *differently rounded* number is worse: `ByteCountFormatter` is 1000-based,
//  so a 2 GiB cleanup came out as an estimate of "2.0 GB" and a result of
//  "2.15 GB", and the honest reading of that pair is that 150 MB went missing.
//  A raw count at least looks unfinished instead of looking finished and wrong.
//
//  That argument is about disagreement, not about Swift. A port that agrees on
//  every input removes the disagreement, and agreement is checkable rather than
//  asserted: `tests/fixtures/human_bytes_golden.tsv` is the contract, Rust's
//  `golden_fixture_pins_the_shared_convention` makes the file Rust's output by
//  definition, and `GlomerisByteFormatTests` holds this implementation to the
//  same file. Either side moving fails in the language that moved.
//
//  ============================================================================
//  WHAT THIS IS NOT
//  ============================================================================
//  Not policy, and not a decision of any kind. It maps a number to a string. It
//  imports Foundation only, exposes one pure static function, reads no state and
//  cannot reach the CLI. `ByteCountFormatter` is deliberately absent, here and
//  everywhere else in this target — it is the thing this file exists to avoid.
//

import Foundation

/// Renders a byte count the way `reporting::bytes::human_bytes` does: binary
/// (1024-based) units labelled KB/MB/GB/TB/PB, one decimal place once scaled,
/// and a bare integer below 1024 because a fractional digit on a whole-byte
/// count is noise rather than precision.
enum GlomerisByteFormat {
    /// Same six labels, same order, as Rust's `UNITS`. The loop stops at the
    /// last one, so a count that would scale past PB saturates there instead of
    /// running off the end of the table.
    private static let units = ["B", "KB", "MB", "GB", "TB", "PB"]

    static func human(_ bytes: UInt64) -> String {
        if bytes < 1024 {
            return "\(bytes) B"
        }

        var value = Double(bytes)
        var unitIndex = 0
        while value >= 1024.0 && unitIndex < units.count - 1 {
            value /= 1024.0
            unitIndex += 1
        }

        // `locale: nil` is load-bearing, not boilerplate. Rust's `{:.1}` always
        // writes a full stop; a localised formatter would write a comma under a
        // European locale and the two languages would disagree for a reason no
        // fixture on an en_US CI runner would ever surface. The rounding of a
        // tie also has to match: C's `%.1f` and Rust's `{:.1}` both round half
        // to even on an exactly representable tie, which is why the fixture
        // pins 1280 bytes at "1.2 KB" rather than "1.3 KB".
        return String(format: "%.1f", locale: nil, value) + " \(units[unitIndex])"
    }
}
