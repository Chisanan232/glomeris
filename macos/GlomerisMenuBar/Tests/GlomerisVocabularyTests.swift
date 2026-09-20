//
//  GlomerisVocabularyTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1306. Three kinds of assertion live here, and the second and
//  third are the ones worth reading:
//
//    1. Coverage — every token the Rust source can emit has wording, and
//       no token silently falls through to the unrecognised fallback.
//       Each list below is transcribed from the Rust enum named in the
//       comment above it, so if Rust grows a variant and this app does
//       not, the new variant is the one thing these tests cannot catch —
//       which is exactly why the fallback is also asserted to be
//       non-reassuring.
//
//    2. The three axes stay separate. `storageImpact` is asserted to be
//       tone-neutral across a wide range of sizes, including absurd ones,
//       because "big number" must never render as "dangerous" and "small
//       number" must never render as "safe". This is the mechanical form
//       of the ticket's design principle.
//
//    3. Colour is never the sole indication. Within each badge axis every
//       term is asserted to have a distinct SF Symbol, so the states stay
//       distinguishable in greyscale — and each symbol name is resolved
//       against the real SF Symbols catalogue, because a mistyped symbol
//       name is not a compile error: it renders as nothing at all, which
//       is precisely the "present but blank" failure HORO-1294 measured on
//       the menu-bar icon.
//

import AppKit
import XCTest

final class GlomerisVocabularyTests: XCTestCase {
    // MARK: - Coverage

    /// `src/monitor/pressure.rs` — `PressureState::as_str`. All five, not
    /// the three HORO-1306's AC #2 names.
    private static let pressureTokens = [
        "HEALTHY", "WARN", "PRESSURED", "CRITICAL", "EMERGENCY",
    ]

    /// `src/reporting/policy_label.rs` — `PolicyLabel::as_str`.
    private static let safetyTokens = [
        "AUTO_SAFE", "ASK", "PROTECTED", "UNKNOWN_INCOMPLETE",
    ]

    /// `src/reporting/dto.rs` — `completeness_tag`.
    private static let completenessTokens = ["complete", "partial", "failed"]

    /// `src/reporting/dto.rs` — `confidence_tag`.
    private static let confidenceTokens = ["high", "medium", "low"]

    /// `src/reporting/dto.rs` — `regenerability_tag`.
    private static let regenerabilityTokens = [
        "regenerable_by_tool", "regenerable_by_rebuild", "not_regenerable", "unknown",
    ]

    /// `src/reporting/dto.rs` — `ExecuteReport::outcome`. `dry_run` is
    /// included: `execute --dry-run` emits it, even though the audit log
    /// never records it.
    private static let outcomeTokens = [
        "succeeded", "failed", "aborted_by_revalidation", "dry_run",
    ]

    /// `src/reporting/dto.rs` — `ExecuteRefusalReport::reason`, including
    /// the pre-resolution `busy` case emitted by `main.rs`.
    private static let refusalTokens = [
        "resource_not_found", "action_not_found", "action_mismatch", "protected",
        "ask_no_consent", "ask_consent_mismatch", "auto_safe_contract_violation", "busy",
    ]

    /// `src/monitor/persistence.rs` — `AuditRecord::source`.
    private static let sourceTokens = ["execute", "free", "emergency"]

    /// `src/evidence/model.rs` — `ResourceKind::tag`.
    private static let kindTokens = [
        "xcode_derived_data", "homebrew_cache", "cargo_target_dir", "cargo_registry_cache",
        "node_modules", "node_package_manager_cache", "docker_build_cache",
        "docker_image_cache", "unknown",
    ]

    /// `src/policy/class.rs` — `ReasonCode::as_str`, all 18.
    private static let reasonTokens = [
        "protected_credential_material", "protected_git_internals", "protected_infra_state",
        "protected_persistent_volume", "protected_user_documents", "protected_system_path",
        "protected_unsafe_mount_or_symlink", "protected_unknown_resource_kind",
        "evidence_incomplete", "evidence_stale", "evidence_probe_failed",
        "resource_in_active_use", "git_worktree_dirty", "rebuild_cost_high",
        "owning_tool_live", "evidence_fresh_and_complete", "regenerable_by_tool",
        "no_active_use_observed",
    ]

    /// Every axis, as (name, tokens, lookup) so the shared invariants
    /// below can be asserted once rather than ten times.
    private static var allAxes: [(name: String, tokens: [String], lookup: (String) -> GlomerisTerm)] {
        [
            ("pressure", pressureTokens, GlomerisVocabulary.pressure),
            ("safety", safetyTokens, GlomerisVocabulary.safety),
            ("completeness", completenessTokens, GlomerisVocabulary.completeness),
            ("confidence", confidenceTokens, GlomerisVocabulary.confidence),
            ("regenerability", regenerabilityTokens, GlomerisVocabulary.regenerability),
            ("outcome", outcomeTokens, GlomerisVocabulary.outcome),
            ("refusal", refusalTokens, GlomerisVocabulary.refusal),
            ("actionSource", sourceTokens, GlomerisVocabulary.actionSource),
            ("kind", kindTokens, GlomerisVocabulary.kind),
            ("reason", reasonTokens, GlomerisVocabulary.reason),
        ]
    }

    /// The axes rendered as chips. `kind` and `reason` are prose and
    /// deliberately carry no symbol — see their doc comments.
    private static var badgeAxes: [(name: String, tokens: [String], lookup: (String) -> GlomerisTerm)] {
        allAxes.filter { $0.name != "kind" && $0.name != "reason" }
    }

    /// The fallback's marker symbol. A term carrying this is, by
    /// construction, one the tables did not recognise.
    private static let unrecognisedSymbol = "questionmark.diamond.fill"

    func testEveryKnownTokenHasRealWordingRatherThanTheFallback() {
        for axis in Self.allAxes {
            for token in axis.tokens {
                let term = axis.lookup(token)
                XCTAssertEqual(
                    term.token,
                    token,
                    "\(axis.name): the raw token must be preserved verbatim for \(token)"
                )
                XCTAssertFalse(
                    term.title.isEmpty,
                    "\(axis.name): \(token) has no title"
                )
                XCTAssertFalse(
                    term.explanation.isEmpty,
                    "\(axis.name): \(token) has no explanation"
                )
                XCTAssertNotEqual(
                    term.symbolName,
                    Self.unrecognisedSymbol,
                    """
                    \(axis.name): \(token) fell through to the unrecognised \
                    fallback. It is a real token in the Rust source, so it \
                    needs real wording.
                    """
                )
            }
        }
    }

    /// The primary wording must be readable without the raw token — a user
    /// should not need documentation to understand what they are looking
    /// at (this ticket's AC #3). The failure mode being guarded against is
    /// a title that is really the enum tag wearing a hat: `snake_case`, or
    /// `SCREAMING_CASE`, or otherwise still spelled like an identifier.
    ///
    /// Deliberately NOT asserted: that a title differs from its token as a
    /// *word*. `PROTECTED` -> "Protected" and `failed` -> "Failed" are
    /// correct translations — those tokens were already English, and
    /// inventing a synonym to satisfy a test would make the copy worse. The
    /// spelling is what distinguishes a tag from a word here.
    func testNoTitleIsStillSpelledLikeAnIdentifier() {
        for axis in Self.allAxes {
            for token in axis.tokens where token != "node_modules" {
                let title = axis.lookup(token).title
                XCTAssertFalse(
                    title.contains("_"),
                    "\(axis.name): \(token)'s title is still snake_case: \(title)"
                )
                XCTAssertNotEqual(
                    title,
                    title.uppercased(),
                    "\(axis.name): \(token)'s title is still SCREAMING_CASE: \(title)"
                )
            }
        }
    }

    /// `node_modules` is the one legitimate exception to the rule above:
    /// its plain-language name genuinely *is* `node_modules`, because that
    /// is what the directory is called and what every JavaScript developer
    /// calls it. Asserted explicitly so the exception is deliberate and
    /// visible rather than an accident of the filter above.
    func testNodeModulesKeepsItsLiteralNameOnPurpose() {
        XCTAssertEqual(GlomerisVocabulary.kind("node_modules").title, "node_modules")
    }

    func testUnrecognisedTokenIsNeverPresentedAsReassurance() {
        for axis in Self.allAxes {
            let term = axis.lookup("a_token_from_a_newer_cli")
            XCTAssertEqual(
                term.token,
                "a_token_from_a_newer_cli",
                "\(axis.name): the unknown raw token must still be shown so it can be reported"
            )
            XCTAssertEqual(
                term.tone,
                .unknown,
                "\(axis.name): an unrecognised token must read as unknown"
            )
            XCTAssertNotEqual(
                term.tone,
                .positive,
                "\(axis.name): an unrecognised token must never read as fine"
            )
            XCTAssertFalse(term.title.isEmpty, "\(axis.name): the fallback needs a title")
        }
    }

    // MARK: - The three axes stay separate

    /// The mechanical form of this ticket's "separate storage impact from
    /// policy/safety from evidence confidence".
    ///
    /// A 40 GB AUTO_SAFE target directory is the most valuable thing on
    /// the list; a 2 MB PROTECTED credential store is still untouchable.
    /// If size carried a tone, the badge would say the opposite of both.
    func testStorageImpactToneIsAlwaysNeutralRegardlessOfSize() {
        let sizes = [
            "0 bytes", "1 KB", "512 KB", "9.9 MB", "1.2 GB", "47 GB", "980 GB", "3.1 TB",
        ]
        for size in sizes {
            for isLowerBound in [true, false] {
                let term = GlomerisVocabulary.storageImpact(human: size, isLowerBound: isLowerBound)
                XCTAssertEqual(
                    term.tone,
                    .neutral,
                    """
                    storage impact for \(size) is toned \(term.tone) — size is an \
                    opportunity axis and must never colour itself into a safety claim.
                    """
                )
            }
        }
        XCTAssertEqual(
            GlomerisVocabulary.storageImpact(human: nil, isLowerBound: false).tone,
            .neutral
        )
    }

    func testStorageImpactMarksALowerBoundAndSaysWhy() {
        let bounded = GlomerisVocabulary.storageImpact(human: "1.2 GB", isLowerBound: true)
        XCTAssertTrue(bounded.title.hasPrefix("\u{2265}"), "a floor must be marked as a floor")
        XCTAssertTrue(
            bounded.explanation.contains("higher"),
            "the explanation must say the real figure is higher, not just show a symbol"
        )

        let exact = GlomerisVocabulary.storageImpact(human: "1.2 GB", isLowerBound: false)
        XCTAssertEqual(exact.title, "1.2 GB")
        XCTAssertFalse(exact.title.contains("\u{2265}"))
    }

    func testStorageImpactWithNoMeasurementSaysSoRatherThanShowingZero() {
        for missing in [nil, ""] as [String?] {
            let term = GlomerisVocabulary.storageImpact(human: missing, isLowerBound: false)
            XCTAssertEqual(term.title, "Size unknown")
            XCTAssertFalse(
                term.title.contains("0"),
                "an unmeasured size must not render as a number a user could act on"
            )
        }
    }

    /// Each vocabulary must announce itself as its own axis, so a chip
    /// never reads as though it belonged to a different one. In particular
    /// safety, evidence confidence and storage impact must not share an
    /// axis name.
    func testEachAxisHasADistinctName() {
        let axisNames = [
            GlomerisVocabulary.pressureAxis,
            GlomerisVocabulary.safetyAxis,
            GlomerisVocabulary.completenessAxis,
            GlomerisVocabulary.confidenceAxis,
            GlomerisVocabulary.regenerabilityAxis,
            GlomerisVocabulary.impactAxis,
            GlomerisVocabulary.outcomeAxis,
            GlomerisVocabulary.refusalAxis,
            GlomerisVocabulary.sourceAxis,
            GlomerisVocabulary.kindAxis,
            GlomerisVocabulary.reasonAxis,
        ]
        XCTAssertEqual(
            Set(axisNames).count,
            axisNames.count,
            "two vocabularies share an axis name, so their chips would read as the same thing"
        )
    }

    func testEveryTermReportsItsOwnAxis() {
        let expected: [String: String] = [
            "pressure": GlomerisVocabulary.pressureAxis,
            "safety": GlomerisVocabulary.safetyAxis,
            "completeness": GlomerisVocabulary.completenessAxis,
            "confidence": GlomerisVocabulary.confidenceAxis,
            "regenerability": GlomerisVocabulary.regenerabilityAxis,
            "outcome": GlomerisVocabulary.outcomeAxis,
            "refusal": GlomerisVocabulary.refusalAxis,
            "actionSource": GlomerisVocabulary.sourceAxis,
            "kind": GlomerisVocabulary.kindAxis,
            "reason": GlomerisVocabulary.reasonAxis,
        ]
        for axis in Self.allAxes {
            for token in axis.tokens {
                XCTAssertEqual(
                    axis.lookup(token).axis,
                    expected[axis.name],
                    "\(axis.name): \(token) reports the wrong axis"
                )
            }
        }
    }

    // MARK: - Colour is never the sole indication

    /// Within one axis, two states must never be distinguishable only by
    /// colour. `CRITICAL` and `EMERGENCY` share the `critical` tone on
    /// purpose — both are red — so their symbols and titles are what keep
    /// them apart, and this test is what makes that non-negotiable.
    func testEveryBadgeAxisUsesADistinctSymbolPerState() {
        for axis in Self.badgeAxes {
            let symbols = axis.tokens.compactMap { axis.lookup($0).symbolName }
            XCTAssertEqual(
                symbols.count,
                axis.tokens.count,
                "\(axis.name): every badge state needs a symbol"
            )
            XCTAssertEqual(
                Set(symbols).count,
                symbols.count,
                """
                \(axis.name): two states share a symbol, so they would be \
                distinguishable by colour alone: \(symbols)
                """
            )
        }
    }

    /// A mistyped SF Symbol name is not a compile error — it renders as
    /// nothing, which is the same silent "present but blank" failure mode
    /// HORO-1294 measured on the menu-bar icon. Resolving each name
    /// against the real catalogue is the only way to catch it.
    func testEverySymbolNameResolvesToARealSFSymbol() {
        var names = Set<String>()
        for axis in Self.allAxes {
            for token in axis.tokens {
                if let symbol = axis.lookup(token).symbolName {
                    names.insert(symbol)
                }
            }
            if let fallback = axis.lookup("definitely_not_a_token").symbolName {
                names.insert(fallback)
            }
        }
        names.insert(
            GlomerisVocabulary.storageImpact(human: "1 GB", isLowerBound: false).symbolName ?? ""
        )

        XCTAssertFalse(names.isEmpty, "no symbol names were collected — the sweep is vacuous")
        for name in names.sorted() {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so it would render as nothing at all"
            )
        }
    }

    /// PROTECTED is the system working correctly, not an error. Colouring
    /// it like a failure would teach a user that Glomeris is broken every
    /// time it refuses to delete their Terraform state.
    func testProtectedReadsAsDeliberateRatherThanAsAFailure() {
        XCTAssertEqual(GlomerisVocabulary.safety("PROTECTED").tone, .guarded)
        XCTAssertNotEqual(GlomerisVocabulary.safety("PROTECTED").tone, .critical)
        XCTAssertNotEqual(GlomerisVocabulary.safety("PROTECTED").tone, .warning)
    }

    /// Only the AUTO_SAFE class may read as positive. ASK is a caution,
    /// PROTECTED is held back, and an evidence gap is unknown — none of
    /// the three may render with the reassuring tone.
    func testOnlyAutoSafeReadsAsPositive() {
        XCTAssertEqual(GlomerisVocabulary.safety("AUTO_SAFE").tone, .positive)
        for token in ["ASK", "PROTECTED", "UNKNOWN_INCOMPLETE"] {
            XCTAssertNotEqual(
                GlomerisVocabulary.safety(token).tone,
                .positive,
                "\(token) must not read as reassuring"
            )
        }
    }

    // MARK: - VoiceOver

    /// A badge is a symbol plus a short title, so its accessibility label
    /// has to carry the subject too — "Asks first" on its own tells a
    /// VoiceOver user nothing about what asks, or about what.
    func testAccessibilityLabelNamesTheAxisAndExplains() {
        let term = GlomerisVocabulary.safety("ASK")
        XCTAssertTrue(term.accessibilityLabel.hasPrefix("\(GlomerisVocabulary.safetyAxis):"))
        XCTAssertTrue(term.accessibilityLabel.contains(term.title))
        XCTAssertTrue(term.accessibilityLabel.contains(term.explanation))
    }

    func testEveryTermHasANonEmptyAccessibilityLabel() {
        for axis in Self.allAxes {
            for token in axis.tokens {
                let label = axis.lookup(token).accessibilityLabel
                XCTAssertFalse(label.isEmpty, "\(axis.name): \(token) has an empty label")
                XCTAssertTrue(
                    label.contains(":"),
                    "\(axis.name): \(token)'s label does not name its axis"
                )
            }
        }
    }

    // MARK: - Wording quality

    /// Popover copy is read at a glance in a ~340pt column. A title that
    /// wraps to three lines defeats the point of having one.
    func testTitlesAreShortEnoughForAChip() {
        for axis in Self.allAxes {
            for token in axis.tokens {
                let title = axis.lookup(token).title
                XCTAssertLessThanOrEqual(
                    title.count,
                    34,
                    "\(axis.name): \(token)'s title is too long for a chip: \(title)"
                )
            }
        }
    }

    /// Explanations are sentences shown to a non-expert. They must not
    /// simply re-emit the internal tag the title was supposed to replace.
    func testExplanationsDoNotLeakInternalTags() {
        for axis in Self.allAxes {
            for token in axis.tokens where token.contains("_") {
                XCTAssertFalse(
                    axis.lookup(token).explanation.contains(token),
                    "\(axis.name): \(token)'s explanation repeats the raw tag"
                )
            }
        }
    }
}
