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

    /// `src/policy/class.rs` — `ReasonCode::as_str`, all 20.
    private static let reasonTokens = [
        "protected_credential_material", "protected_git_internals", "protected_infra_state",
        "protected_persistent_volume", "protected_user_documents", "protected_system_path",
        "protected_unsafe_mount_or_symlink", "protected_unknown_resource_kind",
        "evidence_incomplete", "evidence_stale", "evidence_probe_failed",
        "resource_in_active_use", "git_worktree_dirty", "rebuild_cost_high",
        "regenerability_unknown", "recoverability_irreversible",
        "owning_tool_live", "evidence_fresh_and_complete", "regenerable_by_tool",
        "no_active_use_observed",
    ]

    /// `src/reporting/dto.rs` — `stop_reason_tag`, all seven. HORO-1506, plus
    /// `stopped_by_user` from HORO-1509's cooperative stop and
    /// `envelope_refused` from HORO-1510's bounded Autopilot.
    private static let stopReasonTokens = [
        "target_reached", "safe_exhausted", "budget_exceeded", "no_progress", "stopped_by_user",
        "envelope_refused", "error",
    ]

    /// `src/autopilot/gate.rs` — `RefusalReason::as_str`, all ten. HORO-1510.
    ///
    /// A different enum from `refusalTokens` above despite the shared Rust
    /// name: that one is why `execute` refused, this one is why the Autopilot
    /// envelope would not allow a candidate.
    private static let autopilotRefusalTokens = [
        "autopilot_revoked", "kind_not_allowed", "protected_refused",
        "unknown_incomplete_refused", "ask_not_preauthorized", "action_budget_exhausted",
        "time_budget_exhausted", "byte_budget_exhausted", "reclaim_size_unknown",
        "disk_pressure_too_low",
    ]

    /// `src/executor/goal.rs` — `GoalRejection::as_str`, all three. HORO-1506.
    private static let goalRejectionTokens = [
        "not_finite", "out_of_range", "not_an_improvement",
    ]

    /// `src/monitor/episode.rs` — `EpisodeResponse::as_str`, all three.
    /// HORO-1508. These are the buttons on the pressure notification, and the
    /// set the app offers is read from `PressureStatusReport.responses` rather
    /// than from this list — which is here so the wording is swept like every
    /// other vocabulary.
    private static let episodeResponseTokens = [
        "review_and_recover", "remind_later", "ignore_episode",
    ]

    /// `src/monitor/episode.rs` — `EpisodeRejection::as_str`, both. HORO-1508.
    private static let episodeRejectionTokens = [
        "no_open_episode", "no_notification_due",
    ]

    /// `src/workspace/group.rs` — `ActivityState::tag`, all three. HORO-1511.
    private static let worktreeActivityTokens = ["in_use", "idle", "unknown"]

    /// `src/workspace/branch.rs` — `UpstreamState::tag`, all three. HORO-1511.
    private static let worktreeUpstreamTokens = ["untracked", "tracking", "unknown"]

    /// `src/workspace/branch.rs` — `MergedState::tag`, all three. HORO-1511.
    private static let worktreeMergedTokens = ["merged", "not_merged", "unknown"]

    /// The three developer-workspace axes, which share a token (`unknown`)
    /// and must not share a reading of it.
    private static var worktreeAxes:
        [(name: String, tokens: [String], lookup: (String) -> GlomerisTerm)]
    {
        [
            ("worktreeActivity", worktreeActivityTokens, GlomerisVocabulary.worktreeActivity),
            ("worktreeUpstream", worktreeUpstreamTokens, GlomerisVocabulary.worktreeUpstream),
            ("worktreeMerged", worktreeMergedTokens, GlomerisVocabulary.worktreeMerged),
        ]
    }

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
            ("stopReason", stopReasonTokens, GlomerisVocabulary.stopReason),
            ("goalRejection", goalRejectionTokens, GlomerisVocabulary.goalRejection),
            ("episodeResponse", episodeResponseTokens, GlomerisVocabulary.episodeResponse),
            ("episodeRejection", episodeRejectionTokens, GlomerisVocabulary.episodeRejection),
            ("autopilotRefusal", autopilotRefusalTokens, GlomerisVocabulary.autopilotRefusal),
        ] + worktreeAxes
    }

    /// The axes rendered as chips. `kind`, `reason` and the three
    /// developer-workspace vocabularies are prose and deliberately carry no
    /// symbol — see their doc comments. For the worktree three the absence is
    /// load-bearing rather than cosmetic, and
    /// `testTheWorktreeAxesAreProseSoNoBranchStateCanRenderAsClearance` says why.
    private static var badgeAxes: [(name: String, tokens: [String], lookup: (String) -> GlomerisTerm)] {
        let prose: Set<String> = [
            "kind", "reason", "worktreeActivity", "worktreeUpstream", "worktreeMerged",
        ]
        return allAxes.filter { !prose.contains($0.name) }
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
            GlomerisVocabulary.stopReasonAxis,
            GlomerisVocabulary.goalRejectionAxis,
            GlomerisVocabulary.episodeResponseAxis,
            GlomerisVocabulary.episodeRejectionAxis,
            GlomerisVocabulary.worktreeActivityAxis,
            GlomerisVocabulary.worktreeUpstreamAxis,
            GlomerisVocabulary.worktreeMergedAxis,
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
            "stopReason": GlomerisVocabulary.stopReasonAxis,
            "goalRejection": GlomerisVocabulary.goalRejectionAxis,
            "episodeResponse": GlomerisVocabulary.episodeResponseAxis,
            "episodeRejection": GlomerisVocabulary.episodeRejectionAxis,
            "autopilotRefusal": GlomerisVocabulary.autopilotRefusalAxis,
            "worktreeActivity": GlomerisVocabulary.worktreeActivityAxis,
            "worktreeUpstream": GlomerisVocabulary.worktreeUpstreamAxis,
            "worktreeMerged": GlomerisVocabulary.worktreeMergedAxis,
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

    // MARK: - Resolved command-line tool (HORO-1466)

    private static let cliStates: [GlomerisCliExpectation] = [
        .matches,
        .differs(expected: String(repeating: "ab", count: 32)),
        .notDeclared,
        .notComparable,
    ]

    /// The ticket's AC: the card must state WHICH binary is in use, not merely
    /// that something is wrong. "Glomeris may be running an old version" is not
    /// actionable, and is in effect what the product said before this ticket —
    /// the path existed only inside the error raised when nothing was found.
    ///
    /// Asserted for every state including the healthy one, because a user who
    /// only ever sees a path when something is broken has no way to know what
    /// normal looks like.
    func testEveryCliExpectationNamesTheResolvedPath() {
        let path = "/opt/homebrew/bin/glomeris"
        for state in Self.cliStates {
            let term = GlomerisVocabulary.cliExpectation(state, path: path)
            XCTAssertTrue(
                term.explanation.contains(path),
                "\(state) must name the binary it is talking about: \(term.explanation)"
            )
        }
    }

    /// Four states that call for four different responses — nothing, replace
    /// the tool, nothing (it is a developer build), and look at the file's
    /// permissions. Sharing wording between any two would tell a user to do the
    /// wrong thing, so titles and explanations must all be distinct.
    func testTheFourCliExpectationsAreAllDistinguishable() {
        let terms = Self.cliStates.map {
            GlomerisVocabulary.cliExpectation($0, path: "/usr/local/bin/glomeris")
        }
        XCTAssertEqual(Set(terms.map(\.title)).count, terms.count)
        XCTAssertEqual(Set(terms.map(\.explanation)).count, terms.count)
        XCTAssertEqual(Set(terms.map(\.token)).count, terms.count)
    }

    /// Only a real mismatch is a warning.
    ///
    /// `notDeclared` is the normal state for every locally built app, since one
    /// embeds no CLI and stamps no expected hash. Colouring it as a problem
    /// would make the warning permanent for the one audience that reads this
    /// card, and a permanent warning is one nobody reads — which would cost
    /// exactly the signal this ticket adds.
    func testOnlyAGenuineMismatchIsTonedAsAWarning() {
        let path = "/usr/local/bin/glomeris"
        XCTAssertEqual(GlomerisVocabulary.cliExpectation(.matches, path: path).tone, .positive)
        XCTAssertEqual(
            GlomerisVocabulary.cliExpectation(
                .differs(expected: String(repeating: "ab", count: 32)),
                path: path
            ).tone,
            .warning
        )
        for cannotSay: GlomerisCliExpectation in [.notDeclared, .notComparable] {
            XCTAssertEqual(
                GlomerisVocabulary.cliExpectation(cannotSay, path: path).tone,
                .unknown,
                "\(cannotSay) is 'cannot say', which is not the same as 'bad'"
            )
        }
    }

    /// The whole hash is 64 characters and the row is ~260pt wide, so the
    /// expected value is not what the badge sentence carries — the card renders
    /// it as its own row. A sentence quoting it would be truncated mid-hash,
    /// which is worse than not showing it.
    func testTheMismatchSentenceDoesNotTryToQuoteTheWholeHash() {
        let expected = String(repeating: "ab", count: 32)
        let term = GlomerisVocabulary.cliExpectation(
            .differs(expected: expected),
            path: "/usr/local/bin/glomeris"
        )
        XCTAssertFalse(term.explanation.contains(expected))
    }

    /// Two `glomeris` binaries differing only by directory is the ordinary
    /// case, not an exotic one, so the directory is the fact worth carrying.
    func testCliSourceNamesTheDirectoryItFoundTheBinaryIn() {
        XCTAssertTrue(
            GlomerisVocabulary.cliSource(.pathEntry("/usr/local/bin")).contains("/usr/local/bin")
        )
        XCTAssertTrue(
            GlomerisVocabulary.cliSource(.knownInstallDirectory("/opt/homebrew/bin"))
                .contains("/opt/homebrew/bin")
        )

        let phrases = [
            GlomerisVocabulary.cliSource(.bundled),
            GlomerisVocabulary.cliSource(.pathEntry("/usr/local/bin")),
            GlomerisVocabulary.cliSource(.knownInstallDirectory("/usr/local/bin")),
        ]
        // The last two resolve to the same directory by different rules, and
        // that difference matters: one is on the user's PATH and one was found
        // only because the app knows to look there.
        XCTAssertEqual(Set(phrases).count, phrases.count)
        for phrase in phrases {
            XCTAssertFalse(phrase.isEmpty)
            XCTAssertFalse(phrase.contains("_"), "\(phrase) reads like an internal tag")
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

    // MARK: - Why a recovery run stopped (HORO-1506)

    /// The product rule this axis exists to enforce: a run that stopped short
    /// of the goal must say so. Six of the seven stops did not reach it, and a
    /// success word on any of them would tell the user their disk is where
    /// they asked it to be when it is not.
    func testOnlyReachingTheGoalReadsAsSuccess() {
        XCTAssertEqual(GlomerisVocabulary.stopReason("target_reached").tone, .positive)
        for token in Self.stopReasonTokens where token != "target_reached" {
            let term = GlomerisVocabulary.stopReason(token)
            XCTAssertNotEqual(
                term.tone, .positive,
                "\(token) did not reach the goal and must not read as success"
            )
            for word in ["done", "complete", "finished", "success"] {
                XCTAssertFalse(
                    term.title.lowercased().contains(word),
                    "\(token)'s title claims completion: \(term.title)"
                )
            }
            XCTAssertTrue(
                term.explanation.lowercased().contains("goal")
                    || term.explanation.lowercased().contains("stopped"),
                "\(token) must say it stopped, and where that leaves the goal: \(term.explanation)"
            )
        }
    }

    /// `safe_exhausted` is the stop most easily misread as "your disk is
    /// clean". It means nothing safe remained among what this run could see
    /// and was allowed to take, so the wording has to name what is still
    /// there — and must not claim there is nothing.
    func testExhaustingSafeCandidatesIsNotWordedAsNothingLeft() {
        let term = GlomerisVocabulary.stopReason("safe_exhausted")
        let explanation = term.explanation.lowercased()

        XCTAssertTrue(
            explanation.contains("confirmation"),
            "must say what is waiting on the user: \(term.explanation)"
        )
        XCTAssertTrue(
            explanation.contains("protected"),
            "must say protected resources are still there: \(term.explanation)"
        )
        XCTAssertFalse(
            explanation.contains("nothing is left") || explanation.contains("nothing remains"),
            "must not report an empty disk it did not establish: \(term.explanation)"
        )
    }

    /// The seven stops leave seven different next steps, so each explanation
    /// has to be its own. Two stops sharing wording would make the report
    /// decorative.
    func testEveryStopReasonExplainsSomethingDifferent() {
        let explanations = Self.stopReasonTokens.map { GlomerisVocabulary.stopReason($0).explanation }
        XCTAssertEqual(
            Set(explanations).count, explanations.count,
            "two stop reasons say the same thing"
        )
        let titles = Self.stopReasonTokens.map { GlomerisVocabulary.stopReason($0).title }
        XCTAssertEqual(Set(titles).count, titles.count, "two stop reasons share a title")
    }

    // MARK: - Which Autopilot limit refused (HORO-1510)

    /// The whole reason `envelope_refused` is not `safe_exhausted`: an
    /// Autopilot run that spends a budget refused every candidate it saw, which
    /// is indistinguishable from an empty disk unless the wording says whose
    /// limit it was. Shown as "nothing safe left", it would tell a user their
    /// disk is out of opportunities when a run they start themselves has
    /// plenty.
    func testTheEnvelopeStopSaysItIsAutopilotsLimitAndNotTheDisks() {
        let term = GlomerisVocabulary.stopReason("envelope_refused")
        let explanation = term.explanation.lowercased()

        XCTAssertTrue(
            explanation.contains("autopilot"),
            "must name whose limit stopped the run: \(term.explanation)"
        )
        XCTAssertTrue(
            explanation.contains("not a finding about this disk")
                || explanation.contains("yourself"),
            "must point at the recovery the user can still start: \(term.explanation)"
        )
        XCTAssertNotEqual(
            term.explanation, GlomerisVocabulary.stopReason("safe_exhausted").explanation,
            "the two stops it is most important to tell apart must not share wording"
        )
    }

    /// The distinction this axis carries. Three of the ten refusals are policy
    /// deciding on evidence and survive any envelope a user can write; the
    /// other seven are the envelope itself, and are one setting away from
    /// being allowed. Wording the second group like the first tells a user
    /// something is off limits when it is not — and wording the first group
    /// like the second invites them to go looking for a setting that does not
    /// exist.
    func testAutopilotRefusalsSeparatePolicyFromTheEnvelope() {
        // `reclaim_size_unknown` belongs with the policy group: it is the
        // fail-closed rule about unmeasurable evidence, not a budget that was
        // set too low, even though it is reached while charging a budget.
        let decidedByPolicy = [
            "protected_refused", "unknown_incomplete_refused", "reclaim_size_unknown",
        ]
        for token in decidedByPolicy {
            let explanation = GlomerisVocabulary.autopilotRefusal(token).explanation.lowercased()
            XCTAssertTrue(
                explanation.contains("policy"),
                "\(token) is policy refusing on evidence and must say so: \(explanation)"
            )
        }

        for token in Self.autopilotRefusalTokens where !decidedByPolicy.contains(token) {
            let term = GlomerisVocabulary.autopilotRefusal(token)
            XCTAssertTrue(
                term.explanation.lowercased().contains("autopilot")
                    || term.explanation.lowercased().contains("you"),
                "\(token) is a limit on this run, not on the resource: \(term.explanation)"
            )
            XCTAssertNotEqual(
                term.tone, .warning,
                """
                \(token) is one setting away from being allowed, so it must not \
                read as gravely as a policy refusal
                """
            )
        }
    }

    /// No refusal may read as a completed search, for the same reason
    /// `safe_exhausted` may not: Autopilot declining to take something is not a
    /// finding that there was nothing to take.
    func testNoAutopilotRefusalClaimsTheDiskIsEmptyOrTheRunSucceeded() {
        for token in Self.autopilotRefusalTokens {
            let term = GlomerisVocabulary.autopilotRefusal(token)
            XCTAssertNotEqual(
                term.tone, .positive,
                "\(token) stopped short of the goal and must not read as success"
            )
            let explanation = term.explanation.lowercased()
            for claim in ["nothing is left", "nothing remains", "nothing left", "disk is clean"] {
                XCTAssertFalse(
                    explanation.contains(claim),
                    "\(token) claims an empty disk it did not establish: \(term.explanation)"
                )
            }
        }
        XCTAssertNotEqual(
            GlomerisVocabulary.autopilotRefusal("a_limit_from_a_newer_cli").tone, .positive,
            "even an unrecognised limit stopped the run short of the goal"
        )
    }

    // MARK: - A refused recovery goal (HORO-1506)

    /// A refusal means nothing ran, which is the one fact a user must not have
    /// to infer. The failure being guarded against is a refusal that reads
    /// like a finished run with nothing to do.
    func testEveryGoalRefusalSaysNothingRan() {
        for token in Self.goalRejectionTokens {
            let term = GlomerisVocabulary.goalRejection(token)
            XCTAssertTrue(
                term.explanation.lowercased().contains("nothing ran"),
                "\(token) must say nothing ran: \(term.explanation)"
            )
            XCTAssertNotEqual(
                term.tone, .positive,
                "\(token) refused the request and must not read as success"
            )
        }
        XCTAssertTrue(
            GlomerisVocabulary.goalRejection("a_reason_from_a_newer_cli")
                .explanation.lowercased().contains("nothing ran"),
            "even an unrecognised refusal must say nothing ran"
        )
    }

    /// An already-met goal is a well-formed request to delete nothing, not a
    /// typo, and telling the user to fix their number would be wrong. What it
    /// needs is the one instruction that changes the outcome.
    func testAnAlreadyMetGoalIsExplainedRatherThanTreatedAsAMistake() {
        let alreadyMet = GlomerisVocabulary.goalRejection("not_an_improvement")
        XCTAssertTrue(
            alreadyMet.explanation.lowercased().contains("below current usage"),
            "must say which direction to move the goal: \(alreadyMet.explanation)"
        )

        let outOfRange = GlomerisVocabulary.goalRejection("out_of_range")
        XCTAssertNotEqual(
            alreadyMet.title, outOfRange.title,
            "a goal this disk already meets is not the same as one off the scale"
        )
        XCTAssertNotEqual(alreadyMet.symbolName, outOfRange.symbolName)
    }

    // MARK: - Answers to a pressure alert (HORO-1508)

    /// HORO-1508 forbids an ambiguous "Skip", and this is what makes that
    /// mechanical. Two of the three answers stop the alert, for different
    /// lengths of time, so a word that could label either of them labels
    /// neither: a user who pressed it could not know afterwards which they had
    /// chosen.
    func testNoAnswerIsLabelledWithAWordThatCouldMeanEither() {
        for token in Self.episodeResponseTokens {
            let title = GlomerisVocabulary.episodeResponse(token).title.lowercased()
            for vague in ["skip", "dismiss", "cancel", "ok"] {
                XCTAssertFalse(
                    title.split(whereSeparator: { !$0.isLetter }).contains(Substring(vague)),
                    "\(token)'s title is the ambiguous word \"\(vague)\": \(title)"
                )
            }
        }
    }

    /// The campaign's rule at the one place it is easiest to break: crossing a
    /// threshold must not start deleting anything. The button that leads to
    /// recovery has to say that it only opens the screen, because a
    /// notification is read in a hurry or not at all.
    func testReviewAndRecoverPromisesToOpenRatherThanToDelete() {
        let term = GlomerisVocabulary.episodeResponse("review_and_recover")
        let explanation = term.explanation.lowercased()

        XCTAssertTrue(
            explanation.contains("opens"),
            "must say it opens a screen: \(term.explanation)"
        )
        XCTAssertTrue(
            explanation.contains("nothing is deleted"),
            "must say nothing is deleted yet: \(term.explanation)"
        )
        XCTAssertTrue(
            explanation.contains("until you"),
            "must name the user as the one who starts a run: \(term.explanation)"
        )
    }

    /// AC4 in wording. "Ignore" scoped to one episode is a reasonable answer;
    /// "ignore" read as "stop watching my disk" is a product that silently
    /// stops working. The user cannot tell which they got from the button, so
    /// the explanation has to.
    func testIgnoringOneAlertSaysAnotherWillStillArrive() {
        let ignore = GlomerisVocabulary.episodeResponse("ignore_episode")
        let explanation = ignore.explanation.lowercased()

        XCTAssertTrue(
            ignore.title.lowercased().contains("this"),
            "the title must scope itself to one alert: \(ignore.title)"
        )
        XCTAssertTrue(
            explanation.contains("new episode"),
            "must say a later crossing is a new episode: \(ignore.explanation)"
        )
        XCTAssertTrue(
            explanation.contains("will alert"),
            "must say the new episode still alerts: \(ignore.explanation)"
        )
        XCTAssertFalse(
            explanation.contains("stop monitoring") || explanation.contains("turn off"),
            "must not read as disabling monitoring: \(ignore.explanation)"
        )

        // The snooze is the other answer that stops the alert, and it must not
        // read as the permanent one either.
        XCTAssertTrue(
            GlomerisVocabulary.episodeResponse("remind_later")
                .explanation.lowercased().contains("again"),
            "remind me later must say the alert comes back"
        )
    }

    /// Neither refusal is a malfunction: the ordinary cause of both is that the
    /// disk recovered between the banner appearing and the button being
    /// pressed. Worded as a warning, a user would go looking for a way to make
    /// their choice stick, for a condition that had already resolved itself.
    func testARefusedAnswerReadsAsNewsAboutTheDiskNotAsAnError() {
        for token in Self.episodeRejectionTokens {
            let term = GlomerisVocabulary.episodeRejection(token)
            XCTAssertEqual(
                term.tone, .neutral,
                "\(token) is a benign race and must not be toned as a problem"
            )
            XCTAssertTrue(
                term.explanation.lowercased().contains("nothing was recorded"),
                "\(token) must say the answer was not stored: \(term.explanation)"
            )
            for alarming in ["error", "failed", "invalid"] {
                XCTAssertFalse(
                    term.title.lowercased().contains(alarming),
                    "\(token)'s title reads as a malfunction: \(term.title)"
                )
            }
        }
        XCTAssertTrue(
            GlomerisVocabulary.episodeRejection("a_reason_from_a_newer_cli")
                .explanation.lowercased().contains("monitoring is unaffected"),
            "even an unrecognised refusal must say the monitor is still watching"
        )
    }

    // MARK: - Developer-workspace state (HORO-1511)

    /// The three worktree vocabularies carry no symbol, which is what keeps
    /// them off the coloured axes.
    ///
    /// The one this protects is `merged`. `src/workspace/mod.rs` keeps the
    /// merge facts out of `Evidence` precisely because "already merged" reads as
    /// permission; a green chip saying it, beside a resource whose own policy
    /// class is PROTECTED, would put that reading back on the screen in the one
    /// form a user scans rather than reads. The assertion is `.neutral` AND no
    /// symbol, because either alone would let it become a badge again.
    func testTheWorktreeAxesAreProseSoNoBranchStateCanRenderAsClearance() {
        for axis in Self.worktreeAxes {
            for token in axis.tokens {
                let term = axis.lookup(token)
                XCTAssertNil(
                    term.symbolName,
                    "\(axis.name): \(token) must not render as a chip"
                )
                XCTAssertEqual(
                    term.tone, .neutral,
                    "\(axis.name): \(token) must not carry a tone of its own"
                )
            }
        }
    }

    /// `unknown` appears in all three vocabularies and means a different thing
    /// in each — a failed activity probe, an unmade remote comparison, an
    /// unrecorded default branch. A listener hearing only the title would get
    /// the same three words three times, so each one names its own subject.
    func testTheSharedUnknownTokenReadsDifferentlyInEachWorktreeAxis() {
        let titles = Self.worktreeAxes.map { $0.lookup("unknown").title }
        XCTAssertEqual(
            Set(titles).count, titles.count,
            "the three unknowns are indistinguishable: \(titles)"
        )
        let explanations = Self.worktreeAxes.map { $0.lookup("unknown").explanation }
        XCTAssertEqual(
            Set(explanations).count, explanations.count,
            "the three unknowns explain themselves identically: \(explanations)"
        )
    }

    /// Rust counts every one of these three non-answers as *possible*
    /// outstanding work rather than as an absence of it
    /// (`ActivityState::may_be_in_use`, `UpstreamState::may_hold_unpushed_work`,
    /// `MergedState::Unknown`'s "deliberately not a guess"). Wording that
    /// drifted towards the reassuring end of each scale would undo that on the
    /// way to the screen, and nothing else in this app would notice: the
    /// `holds_work_in_progress` flag the wording sits beside is computed in
    /// Rust and would still be right.
    func testAnUnansweredProbeIsNeverWordedAsTheReassuringAnswer() {
        XCTAssertFalse(
            GlomerisVocabulary.worktreeActivity("unknown").title.lowercased().contains("idle"),
            "an activity probe that could not answer must not be called idle"
        )
        XCTAssertTrue(
            GlomerisVocabulary.worktreeActivity("unknown")
                .explanation.lowercased().contains("not known to be idle"),
            "the explanation must say what it cannot claim"
        )
        XCTAssertTrue(
            GlomerisVocabulary.worktreeUpstream("unknown")
                .explanation.lowercased().contains("cannot be ruled out"),
            "an unmade remote comparison must not read as \"nothing is unpushed\""
        )
        XCTAssertTrue(
            GlomerisVocabulary.worktreeMerged("unknown")
                .explanation.lowercased().contains("does not guess"),
            "an unrecorded default branch must say Glomeris declined to guess at one"
        )
    }

    /// `untracked` is the token most easily misread, and the misreading is the
    /// dangerous direction: it does not mean nothing is unpushed, it means there
    /// is nothing published to compare against. Rust's own doc comment says
    /// "**Not** \"nothing is unpushed\"", and this is that sentence, held to.
    func testNoUpstreamBranchIsNotWordedAsNothingOutstanding() {
        let term = GlomerisVocabulary.worktreeUpstream("untracked")
        XCTAssertTrue(
            term.explanation.lowercased().contains("may exist only on this mac"),
            "untracked must say the commits may be local-only: \(term.explanation)"
        )
        for reassuring in ["nothing unpushed", "all pushed", "up to date", "published"] {
            XCTAssertFalse(
                term.title.lowercased().contains(reassuring),
                "untracked's title claims the comparison it could not make: \(term.title)"
            )
        }
    }

    /// An unrecognised token in any of the three must read as "we do not know
    /// what this is" and keep the raw token, the same rule every other table
    /// follows — checked here per-axis because the fallback is the only arm
    /// these three share and a copied `case` label would route a live token to
    /// the wrong axis's wording without failing the guard script.
    func testAnUnknownWorktreeTokenFallsBackWithinItsOwnAxis() {
        for axis in Self.worktreeAxes {
            let term = axis.lookup("a_state_from_a_newer_cli")
            XCTAssertEqual(term.token, "a_state_from_a_newer_cli")
            XCTAssertEqual(
                term.symbolName, Self.unrecognisedSymbol,
                "\(axis.name): an unrecognised token must be marked as such"
            )
            XCTAssertEqual(
                term.tone, .unknown,
                "\(axis.name): an unrecognised token must not be toned as fine"
            )
            XCTAssertEqual(
                term.axis, axis.lookup("unknown").axis,
                "\(axis.name): the fallback belongs to a different axis than the table"
            )
        }
    }
}
