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

    /// `src/reporting/policy_label.rs` — `PolicyLabel::as_str`. Five, not
    /// four: `NOT_POLICY_GOVERNED` (HORO-1468) is the label on an audit row
    /// for a file Glomeris wrote itself, which no policy decision projects
    /// to. It reaches this app only through the history section.
    private static let safetyTokens = [
        "AUTO_SAFE", "ASK", "PROTECTED", "UNKNOWN_INCOMPLETE", "NOT_POLICY_GOVERNED",
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

    /// `src/monitor/persistence.rs` — `AuditRecord::source`. The two
    /// `autopilot_*` values are written by `src/autopilot/run.rs`
    /// (HORO-1310); this list is transcribed by hand because that field has
    /// no single canonical producer for
    /// `check-vocabulary-covers-cli-tokens.sh` to diff against, which is
    /// precisely how they were missed once already.
    private static let sourceTokens = [
        "execute", "free", "emergency", "autopilot_auto_safe", "autopilot_preauthorized_ask",
    ]

    /// `src/evidence/model.rs` — `ResourceKind::tag`.
    private static let kindTokens = [
        "xcode_derived_data", "homebrew_cache", "cargo_target_dir", "cargo_registry_cache",
        "node_modules", "node_package_manager_cache", "docker_build_cache",
        "docker_image_cache", "unknown",
    ]

    /// `src/actions/llm.rs` — `llm_check_outcome`, all five. HORO-1309.
    private static let llmCheckTokens = [
        "ok", "misconfigured", "unreachable", "rejected", "unusable_response",
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
            ("llmCheck", llmCheckTokens, GlomerisVocabulary.llmCheckOutcome),
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
            GlomerisVocabulary.monitorAxis,
            GlomerisVocabulary.outcomeAxis,
            GlomerisVocabulary.refusalAxis,
            GlomerisVocabulary.sourceAxis,
            GlomerisVocabulary.kindAxis,
            GlomerisVocabulary.reasonAxis,
            GlomerisVocabulary.llmCheckAxis,
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
            "llmCheck": GlomerisVocabulary.llmCheckAxis,
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

    /// The background-monitor terms are keyed on a Bool and a formatted
    /// age rather than on a CLI token, so the token-axis sweeps above do
    /// not reach them. Same two hazards apply: an unreal symbol renders as
    /// nothing, and two states sharing a symbol would leave them
    /// separable by colour alone.
    func testMonitorTermsUseRealAndDistinctSymbols() {
        let terms = [
            GlomerisVocabulary.monitorLoaded(true),
            GlomerisVocabulary.monitorLoaded(false),
            GlomerisVocabulary.monitorHeartbeat(ageDescription: "8s"),
            GlomerisVocabulary.monitorHeartbeat(ageDescription: nil),
        ]

        let symbols = terms.compactMap { $0.symbolName }
        XCTAssertEqual(symbols.count, terms.count, "every monitor state needs a symbol")
        XCTAssertEqual(
            Set(symbols).count,
            symbols.count,
            "two monitor states share a symbol: \(symbols)"
        )
        for name in symbols {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so it would render as nothing at all"
            )
        }
        for term in terms {
            XCTAssertEqual(term.axis, GlomerisVocabulary.monitorAxis)
            XCTAssertFalse(term.title.isEmpty)
            XCTAssertFalse(term.explanation.isEmpty)
            XCTAssertLessThanOrEqual(term.title.count, 34)
        }
    }

    /// An empty age string is the same fact as no age — the CLI reporting
    /// `heartbeat_age_secs: null` and a formatter returning "" must not
    /// produce "ago" with nothing in front of it.
    func testEmptyHeartbeatAgeIsTreatedAsNoCheckIn() {
        XCTAssertEqual(
            GlomerisVocabulary.monitorHeartbeat(ageDescription: ""),
            GlomerisVocabulary.monitorHeartbeat(ageDescription: nil)
        )
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
    /// PROTECTED is held back, an evidence gap is unknown, and a row about
    /// Glomeris's own file is no judgement at all — none of the four may
    /// render with the reassuring tone.
    func testOnlyAutoSafeReadsAsPositive() {
        XCTAssertEqual(GlomerisVocabulary.safety("AUTO_SAFE").tone, .positive)
        for token in ["ASK", "PROTECTED", "UNKNOWN_INCOMPLETE", "NOT_POLICY_GOVERNED"] {
            XCTAssertNotEqual(
                GlomerisVocabulary.safety(token).tone,
                .positive,
                "\(token) must not read as reassuring"
            )
        }
    }

    /// HORO-1468. This label is the one member of the safety axis that is
    /// not a safety verdict, and both ways of getting it wrong are wrong in
    /// the same direction as a lie: a reassuring tone would tell the user
    /// policy cleared the deletion, and an alarming one would tell them
    /// something is wrong with a file Glomeris is entitled to remove.
    ///
    /// Asserted as "not any of the loaded tones" rather than "== .neutral"
    /// alone, so that changing the tone to any judgement-carrying value
    /// fails here rather than only at review.
    func testGlomerisOwnFileIsNeitherAClearanceNorAnAlarm() {
        let term = GlomerisVocabulary.safety("NOT_POLICY_GOVERNED")

        XCTAssertEqual(term.tone, .neutral)
        for loaded: GlomerisTone in [.positive, .caution, .warning, .critical, .guarded, .unknown] {
            XCTAssertNotEqual(
                term.tone,
                loaded,
                "a row about Glomeris's own file must carry no safety judgement"
            )
        }
        // And it must say whose file it was, or the row reads as a verdict
        // on one of the user's resources with the wording left off.
        XCTAssertTrue(
            term.title.contains("Glomeris") || term.explanation.contains("itself"),
            "the wording must say the file was Glomeris's own: \(term.title) / \(term.explanation)"
        )
    }

    // MARK: - AI provider connection test (HORO-1309)

    /// Only a working connection may read as reassuring. The other four are
    /// all things the user has to go and fix, and a green-toned chip on any
    /// of them would say the setup is fine while planning keeps failing.
    func testOnlyASuccessfulConnectionTestReadsAsPositive() {
        XCTAssertEqual(GlomerisVocabulary.llmCheckOutcome("ok").tone, .positive)
        for token in ["misconfigured", "unreachable", "rejected", "unusable_response"] {
            XCTAssertNotEqual(
                GlomerisVocabulary.llmCheckOutcome(token).tone,
                .positive,
                "\(token) must not read as a working setup"
            )
        }
    }

    /// HORO-1299's lesson, asserted rather than only documented: a local
    /// configuration problem must not be described as something the provider
    /// did, and a provider refusal must not be described as something local.
    /// Collapsing the two is what made a BYOK 401 undiagnosable.
    func testAConfigurationProblemAndAProviderRefusalSayDifferentThings() {
        let misconfigured = GlomerisVocabulary.llmCheckOutcome("misconfigured")
        XCTAssertTrue(
            misconfigured.explanation.lowercased().contains("nothing was sent"),
            """
            a local configuration problem must say nothing left this machine, \
            or the user will go looking at the provider: \(misconfigured.explanation)
            """
        )

        let rejected = GlomerisVocabulary.llmCheckOutcome("rejected")
        XCTAssertTrue(
            rejected.explanation.lowercased().contains("answered"),
            "a refusal must say the provider answered: \(rejected.explanation)"
        )

        let unreachable = GlomerisVocabulary.llmCheckOutcome("unreachable")
        XCTAssertFalse(
            unreachable.explanation.lowercased().contains("refus"),
            "no answer at all is not a refusal: \(unreachable.explanation)"
        )

        let explanations = Self.llmCheckTokens.map {
            GlomerisVocabulary.llmCheckOutcome($0).explanation
        }
        XCTAssertEqual(
            Set(explanations).count,
            explanations.count,
            "two connection-test states give the same advice, so one of them is useless"
        )
    }

    /// AC 6: provider configuration stays generic. Wording that named a
    /// vendor would be wrong for every self-hosted, gateway and
    /// corporate-proxy setup — and this app is the surface most likely to
    /// acquire a helpful-sounding "check your OpenAI key".
    func testNoConnectionTestWordingNamesAParticularProvider() {
        let vendors = ["openai", "anthropic", "azure", "ollama", "openrouter", "claude", "gpt"]
        for token in Self.llmCheckTokens + ["a_token_from_a_newer_cli"] {
            let term = GlomerisVocabulary.llmCheckOutcome(token)
            let copy = "\(term.title) \(term.explanation)".lowercased()
            for vendor in vendors {
                XCTAssertFalse(
                    copy.contains(vendor),
                    "\(token)'s wording names \(vendor), which is wrong for every other setup"
                )
            }
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

    // MARK: - Storage-impact tier (HORO-1307)

    /// `src/reporting/impact.rs` — `StorageImpactTier::as_str`. Transcribed
    /// the same way as the axes above; `scripts/check-vocabulary-covers-cli-
    /// tokens.sh` diffs this lookup against that Rust function directly.
    private static let impactTierTokens = ["unknown", "normal", "notable", "large"]

    /// `impactTier` is the one lookup that deliberately returns `nil`, so it
    /// cannot join the `allAxes` sweep. The reason it is Optional is the
    /// property worth pinning: a chip on every row is noise rather than
    /// emphasis, and `"unknown"` would only restate what the size badge
    /// already says ("Size unknown").
    func testOnlyTheNotableTiersGetWording() {
        XCTAssertNil(GlomerisVocabulary.impactTier("normal"))
        XCTAssertNil(GlomerisVocabulary.impactTier("unknown"))
        XCTAssertNil(GlomerisVocabulary.impactTier(nil))
        XCTAssertNotNil(GlomerisVocabulary.impactTier("notable"))
        XCTAssertNotNil(GlomerisVocabulary.impactTier("large"))
    }

    /// Every token Rust can emit is handled deliberately — either with
    /// wording or with a deliberate `nil` — and none of them reaches the
    /// unrecognised fallback.
    func testEveryImpactTierTokenIsHandledDeliberately() {
        for token in Self.impactTierTokens {
            guard let term = GlomerisVocabulary.impactTier(token) else { continue }
            XCTAssertEqual(term.token, token)
            XCTAssertEqual(term.axis, GlomerisVocabulary.impactTierAxis)
            XCTAssertFalse(
                term.title.contains("Unrecognised"),
                "\(token) fell through to the unrecognised fallback"
            )
        }

        let unknownToken = GlomerisVocabulary.impactTier("colossal")
        XCTAssertNotNil(unknownToken, "an unrecognised tier must be surfaced, not silently dropped")
        XCTAssertEqual(unknownToken?.tone, .unknown)
    }

    /// The whole point of HORO-1307's AC 4: size is not safety. A tier must
    /// never render as reassuring or as alarming, because "large" says
    /// nothing about whether the thing may be deleted — a large AUTO_SAFE
    /// candidate is an opportunity and a large PROTECTED one is still
    /// protected.
    func testNoImpactTierCarriesASafetyTone() {
        for token in Self.impactTierTokens {
            guard let term = GlomerisVocabulary.impactTier(token) else { continue }
            XCTAssertEqual(
                term.tone, .neutral,
                "\(token) reads as a safety judgment, but it is a magnitude"
            )
        }
    }

    /// Greyscale and colour-blind legibility: the two tiers that do render
    /// must be distinguishable by symbol, and both symbols must be real —
    /// a mistyped SF Symbol name renders as nothing at all.
    func testImpactTierSymbolsAreRealAndDistinct() {
        let terms = Self.impactTierTokens.compactMap { GlomerisVocabulary.impactTier($0) }
            + [GlomerisVocabulary.impactTier("colossal")].compactMap { $0 }
        let symbols = terms.compactMap { $0.symbolName }

        XCTAssertEqual(symbols.count, terms.count, "every rendered tier needs a symbol")
        XCTAssertEqual(
            Set(symbols).count, symbols.count,
            "two tiers share a symbol, so they would differ by colour alone: \(symbols)"
        )
        for name in symbols {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so it would render as nothing at all"
            )
        }
    }

    /// Same chip-width and no-raw-tag rules the other axes are held to.
    func testImpactTierWordingIsChipSizedAndLeaksNoTags() {
        for token in Self.impactTierTokens {
            guard let term = GlomerisVocabulary.impactTier(token) else { continue }
            XCTAssertLessThanOrEqual(term.title.count, 34, "\(token)'s title is too long for a chip")
            XCTAssertFalse(term.explanation.isEmpty)
            XCTAssertFalse(term.accessibilityLabel.isEmpty)
            XCTAssertTrue(term.accessibilityLabel.contains(":"))
        }
    }

    // MARK: - A refusal may not describe a narrower cause than it covers (HORO-1326)

    /// `ask_consent_mismatch` fires for two causes, and the CLI's own message
    /// names both: the fingerprint is stale, *or* it was observed for a
    /// different resource. The wording here described only the first ("The
    /// resource changed after you confirmed"), so in the second case the app
    /// stated something untrue about the user's filesystem — on a product whose
    /// whole claim is that it reports only what it observed. Nothing changed;
    /// the confirmation simply never belonged to this resource.
    ///
    /// Asserted as a prohibition on the narrower claim, plus the minimum the
    /// sentence must still convey. Deliberately *not* an equality against the
    /// replacement copy: pinning the literal would fail on every legitimate
    /// rewording and would teach the next person to update the fixture instead
    /// of re-reading what the CLI means by the token.
    func testConsentMismatchWordingCoversBothCausesRatherThanOnlyAChange() {
        let term = GlomerisVocabulary.refusal("ask_consent_mismatch")
        let copy = "\(term.title) \(term.explanation)".lowercased()

        // Each of these asserts, or presupposes, that the resource used to
        // match and then changed — true of one cause, false of the other.
        for narrowing in ["changed", "no longer", "expired", "out of date", "stale"] {
            XCTAssertFalse(
                copy.contains(narrowing),
                """
                ask_consent_mismatch's wording says "\(narrowing)", which claims the \
                resource changed. It also fires when a structurally valid \
                confirmation was observed for a different resource, where nothing \
                changed at all: \(term.title) / \(term.explanation)
                """
            )
        }

        // The prohibition above is satisfiable by saying nothing useful, so the
        // sentence must still name what did not match and that nothing ran.
        XCTAssertTrue(
            copy.contains("confirmation"),
            "the user has to know which of their actions this is about: \(copy)"
        )
        XCTAssertTrue(
            copy.contains("match"),
            "a mismatch is the actual fact, and it is what distinguishes this "
                + "refusal from ask_no_consent: \(copy)"
        )
        XCTAssertTrue(
            copy.contains("refused"),
            "the reader must be told the action did not run: \(copy)"
        )
    }

    /// The two consent refusals are different situations with different
    /// remedies — one needs a confirmation, the other needs a fresh one — so
    /// neither the title nor the explanation may be shared between them. A
    /// widened sentence is the most likely way to collapse them by accident.
    func testTheTwoConsentRefusalsDoNotReadAsTheSameSituation() {
        let noConsent = GlomerisVocabulary.refusal("ask_no_consent")
        let mismatch = GlomerisVocabulary.refusal("ask_consent_mismatch")
        XCTAssertNotEqual(noConsent.title, mismatch.title)
        XCTAssertNotEqual(noConsent.explanation, mismatch.explanation)
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
