//
//  AutopilotSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1510 AC 2, standing half: what the panel says about Autopilot on an
//  ordinary day, and the one thing it must never say.
//
//  The claim worth the most here is negative. A card with two visual states —
//  on and off — has nowhere to put "I could not find out", and the fallback it
//  reaches for looks like a finished answer. On this particular card the
//  finished-looking answer is "Autopilot is off", which means *nothing on this
//  Mac deletes anything without asking you* — a reassurance, stated at the
//  moment it is least justified. So the card has three states, and this file is
//  what keeps the third one from being optimised away.
//

import XCTest

final class AutopilotSectionViewTests: XCTestCase {

    // MARK: - Fixtures

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    /// The published fixture: a grant that is on. Decoded rather than written out
    /// here, so this file cannot assert a report shape the CLI does not produce.
    private func enabledReport() throws -> AutopilotEnvelopeDto {
        try JSONDecoder().decode(
            AutopilotEnvelopeDto.self,
            from: try Data(
                contentsOf: Self.fixturesDir.appendingPathComponent(
                    "autopilot_envelope_report.json"
                )
            )
        )
    }

    /// The same grant with the two HORO-1510 fields moved, built field by field
    /// from the fixture so the limits and vocabularies stay the CLI's.
    ///
    /// `startsUnprompted` is passed rather than computed, because computing it
    /// here would be this test file doing the thing the whole ticket forbids the
    /// app from doing.
    private func report(
        enabled: Bool,
        respondToAlerts: Bool,
        startsUnprompted: Bool
    ) throws -> AutopilotEnvelopeDto {
        let base = try enabledReport()
        return AutopilotEnvelopeDto(
            enabled: enabled,
            allowedKinds: base.allowedKinds,
            askPreauthorizations: base.askPreauthorizations,
            maxActions: base.maxActions,
            maxBytes: base.maxBytes,
            maxBytesHuman: base.maxBytesHuman,
            maxDurationSecs: base.maxDurationSecs,
            minPressure: base.minPressure,
            respondToAlerts: respondToAlerts,
            startsUnprompted: startsUnprompted,
            ceilings: base.ceilings,
            allowlistableKinds: base.allowlistableKinds,
            neverAllowlistableKinds: base.neverAllowlistableKinds,
            preauthorizableReasons: base.preauthorizableReasons,
            neverPreauthorizableReasons: base.neverPreauthorizableReasons,
            pressureStates: base.pressureStates,
            neverExecutableLabels: base.neverExecutableLabels,
            aiAuthority: base.aiAuthority,
            storedAt: base.storedAt
        )
    }

    // MARK: - The three states

    func testBeforeTheFirstReadTheCardSaysItIsReading() {
        XCTAssertEqual(
            AutopilotSectionPresentation.state(
                report: nil,
                errorDetail: nil,
                hasLoadedOnce: false
            ),
            .loading
        )
    }

    func testAReadThatReturnedAGrantShowsThatGrant() throws {
        let grant = try enabledReport()

        XCTAssertEqual(
            AutopilotSectionPresentation.state(
                report: grant,
                errorDetail: nil,
                hasLoadedOnce: true
            ),
            .grant(grant)
        )
    }

    /// The load-bearing one. A read that returned without a grant must reach a
    /// state of its own — not `loading`, which would spin forever, and above all
    /// not a rendered grant, because there is no grant to render and the only
    /// value available to render is the absence of one.
    func testAReadThatFailedIsItsOwnStateRatherThanAnAnswer() {
        let state = AutopilotSectionPresentation.state(
            report: nil,
            errorDetail: "glomeris: could not read the envelope",
            hasLoadedOnce: true
        )

        XCTAssertEqual(state, .unreadable("glomeris: could not read the envelope"))
        if case .grant = state {
            XCTFail("a failed read is being presented as a grant")
        }
    }

    /// And a failure with nothing to quote is still that state. A CLI that dies
    /// silently is the case most likely to arrive with no detail, and it is not
    /// the case in which to fall back to something reassuring.
    func testAFailedReadWithNothingToQuoteIsStillUnreadable() {
        XCTAssertEqual(
            AutopilotSectionPresentation.state(
                report: nil,
                errorDetail: nil,
                hasLoadedOnce: true
            ),
            .unreadable(nil)
        )
    }

    /// A later failed read leaves the last good grant on screen. The alternative
    /// is a card that blanks itself every time a poll stumbles, having already
    /// shown the user the answer.
    func testALaterFailureKeepsTheGrantThatWasRead() throws {
        let grant = try enabledReport()

        XCTAssertEqual(
            AutopilotSectionPresentation.state(
                report: grant,
                errorDetail: "transient",
                hasLoadedOnce: true
            ),
            .grant(grant)
        )
    }

    // MARK: - What the failure says

    /// It says what it does not know, in those words. The sentence a user must
    /// not be shown instead is the one about nothing being deleted, and the
    /// wording here is deliberately about the *card's* inability rather than
    /// about the disk.
    func testTheFailureSaysWhatIsUnknownRatherThanSayingNothingWillHappen() {
        let message = AutopilotSectionPresentation.unreadableGrantMessage(nil)

        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("could not read"), message.title)
        XCTAssertTrue(message.title.contains("may start on its own"), message.title)
        XCTAssertFalse(message.title.contains("Autopilot is off"), message.title)
    }

    /// The CLI's own words are carried through when there are any, as one
    /// composed sentence rather than concatenated prose.
    func testTheFailureQuotesTheCliWhenItSaidAnything() {
        XCTAssertEqual(
            AutopilotSectionPresentation.unreadableGrantMessage("exit 2: unrecognized argument")
                .title,
            SpokenLabel.compose([
                AutopilotSectionPresentation.unreadableGrantSentence,
                "exit 2: unrecognized argument",
            ])
        )
    }

    // MARK: - What a listener hears

    /// AC 2, spoken. "Autopilot is on" does not say whether it starts by itself,
    /// and a listener given only that has been told the less important of the two
    /// facts the card is on screen to state.
    func testTheSpokenStateSaysWhetherARunMayStartByItself() throws {
        let grant = try report(enabled: true, respondToAlerts: true, startsUnprompted: true)
        let spoken = AutopilotSectionPresentation.spokenState(
            status: AutopilotStatusViewModel.make(grant),
            mode: AutopilotUnpromptedMode.make(grant)
        )

        XCTAssertTrue(spoken.hasPrefix("Autopilot is on."), spoken)
        XCTAssertTrue(
            spoken.contains(AutopilotUnpromptedMode.startsOnPressure.summary),
            spoken
        )
        XCTAssertTrue(
            spoken.contains(PressureAlertPresentation.unpromptedModeLabel),
            "the mode is a fragment answering a label; heard without it, it could be about "
                + "anything on the card — \(spoken)"
        )
    }

    /// And the two Macs are told apart by what is said. An opted-in Mac and a
    /// default one differing only in a glyph or a tint is the failure campaign
    /// §14 names, and a listener gets neither.
    func testTheOptedInMacAndTheDefaultOneAreSpokenDifferently() throws {
        let spoken = try [
            (enabled: true, respondToAlerts: true, startsUnprompted: true),
            (enabled: true, respondToAlerts: false, startsUnprompted: false),
            (enabled: false, respondToAlerts: true, startsUnprompted: false),
            (enabled: false, respondToAlerts: false, startsUnprompted: false),
        ].map { combination in
            let grant = try report(
                enabled: combination.enabled,
                respondToAlerts: combination.respondToAlerts,
                startsUnprompted: combination.startsUnprompted
            )
            return AutopilotSectionPresentation.spokenState(
                status: AutopilotStatusViewModel.make(grant),
                mode: AutopilotUnpromptedMode.make(grant)
            )
        }

        XCTAssertEqual(Set(spoken).count, 4, "\(spoken)")
    }

    /// The mode's tone and symbol are the mode's, not re-decided here — so the
    /// one state that acts unasked is the one state that is visually marked, and
    /// it is marked by symbol as well as by colour.
    func testTheActingModeIsMarkedBySymbolAsWellAsColour() {
        let acting = AutopilotUnpromptedMode.startsOnPressure
        let inert: [AutopilotUnpromptedMode] = [.askedFirst, .dormantWhileRevoked]

        for mode in inert {
            XCTAssertNotEqual(mode.symbolName, acting.symbolName)
            XCTAssertNotEqual(mode.tone, acting.tone)
        }
    }

    // MARK: - What it does not repeat

    /// The two positional sentences from the Autopilot pane are not rendered
    /// here, and this is the assertion that keeps them out.
    ///
    /// `AutopilotStatusViewModel.detail` says "what is listed below, within these
    /// limits"; `AutopilotUnpromptedMode.detail` says "inside the limits above".
    /// Both are true on the pane and neither is true in the panel, where there is
    /// no list and there are no limits on screen. Reusing them would look like
    /// consistency and would point a reader at a card that is not there.
    func testTheCardDoesNotBorrowThePanesPositionalWording() throws {
        let card = try String(
            contentsOf: URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()
                .deletingLastPathComponent()
                .appendingPathComponent("Sources/AutopilotSectionView.swift"),
            encoding: .utf8
        )

        // The card renders `status.title` and `mode.summary`, both of which name
        // a state and point at nothing.
        XCTAssertTrue(card.contains("status.title"), "the card no longer renders the status title")
        XCTAssertTrue(card.contains("mode.summary"), "the card no longer renders the mode summary")
        XCTAssertFalse(
            card.contains("status.detail"),
            """
            The pane's status detail says "what is listed below, within these limits", and \
            nothing is listed below in the panel (HORO-1510).
            """
        )
        XCTAssertFalse(
            card.contains("mode.detail"),
            """
            The mode's detail says "inside the limits above", and there are no limits above \
            it in the panel (HORO-1510).
            """
        )
        // And it says where they are instead, rather than leaving a reader to
        // assume the panel is the whole story.
        XCTAssertTrue(
            AutopilotSectionPresentation.limitsElsewhere.contains("Autopilot settings"),
            AutopilotSectionPresentation.limitsElsewhere
        )
    }

    /// The card names itself distinctly from every other card in the panel, since
    /// its title is what a listener tabs past.
    func testTheCardTitleIsDistinctFromItsNeighbours() {
        XCTAssertNotEqual(
            AutopilotSectionPresentation.cardTitle, RecoverySectionView.cardTitle
        )
        XCTAssertNotEqual(
            AutopilotSectionPresentation.cardTitle, PressureAlertPresentation.cardTitle
        )
    }
}
