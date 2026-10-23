//
//  GlomerisGoldenJourneyUITests.swift
//  GlomerisMenuBarUITests
//
//  HORO-2176: the founder's golden journey, driven through the real menu-bar
//  popover and candidates window of a DEBUG build.
//
//  The app is launched with GLOMERIS_QA_FIXTURE_CLI pointing at
//  QAFixtures/glomeris-qa-fixture-cli.py, so every screen shows deterministic,
//  clearly-labelled synthetic candidates and nothing on the machine is read,
//  scanned or cleaned. Each test gets its own HOME and state directory.
//
//  Honest limits, stated here so a green run is not over-read:
//   - Increase Contrast and Reduce Motion are system settings XCUITest cannot
//     toggle in isolation; they stay manual (the app has no animation code).
//   - Screenshots are window-level only (never the whole desktop).
//

import Carbon.HIToolbox
import XCTest

final class GlomerisGoldenJourneyUITests: XCTestCase {
    private var app: XCUIApplication!
    private var stateDir: URL!

    // MARK: - Fixture knowledge (independent of the app code under test)

    /// Mirrors LABELS in the fixture CLI: how many of `n` candidates fall in
    /// each class. Restated here on purpose, so the test checks the app's
    /// summary against the data rather than against itself.
    private func expectedSummary(_ n: Int) -> String {
        guard n > 0 else { return "" }
        let cycle = ["safe", "ask", "ask", "prot", "prot", "prot", "prot", "unk", "unk", "unk"]
        var counts = ["safe": 0, "ask": 0, "prot": 0, "unk": 0]
        for i in 0..<n { counts[cycle[i % 10], default: 0] += 1 }
        var parts: [String] = []
        if counts["safe"]! > 0 { parts.append("\(counts["safe"]!) safe to reclaim") }
        if counts["ask"]! > 0 { parts.append("\(counts["ask"]!) ask first") }
        if counts["prot"]! > 0 { parts.append("\(counts["prot"]!) protected") }
        if counts["unk"]! > 0 { parts.append("\(counts["unk"]!) not enough evidence") }
        return "\(n) found: " + parts.joined(separator: ", ")
    }

    private func syntheticId(_ i: Int) -> String {
        let kinds = ["docker_image", "docker_volume", "cargo_target_dir", "node_modules"]
        return "devqa-synthetic:\(kinds[i % 4]):" + String(format: "%05d", i)
    }

    // MARK: - Harness

    private var fixtureCLIPath: String {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("QAFixtures/glomeris-qa-fixture-cli.py").path
    }

    private func launch(count: Int) throws {
        stateDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-uitest-\(UUID().uuidString)")
        let home = stateDir.appendingPathComponent("home")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        app = XCUIApplication()
        app.launchEnvironment["GLOMERIS_QA_FIXTURE_CLI"] = fixtureCLIPath
        app.launchEnvironment["GLOMERIS_QA_COUNT"] = String(count)
        app.launchEnvironment["GLOMERIS_QA_STATE_DIR"] = stateDir.path
        app.launchEnvironment["HOME"] = home.path
        app.launchEnvironment["CFFIXED_USER_HOME"] = home.path
        app.launch()
    }

    override func setUpWithError() throws {
        continueAfterFailure = false
        try XCTSkipUnless(
            FileManager.default.isExecutableFile(atPath: fixtureCLIPath),
            "QA fixture CLI missing or not executable: \(fixtureCLIPath)")
    }

    override func tearDown() {
        // On failure, keep the accessibility tree: it is the evidence for why an
        // element was not found, and costs nothing when the test passed.
        if let run = testRun, !run.hasSucceeded, let app {
            let tree = app.debugDescription
            print("ACCESSIBILITY-TREE-BEGIN \(name)\n\(tree)\nACCESSIBILITY-TREE-END")
            let attachment = XCTAttachment(string: tree)
            attachment.name = "accessibility-tree-on-failure"
            attachment.lifetime = .keepAlways
            add(attachment)
            if let dir = ProcessInfo.processInfo.environment["GLOMERIS_UITEST_ARTIFACT_DIR"] {
                try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
                try? tree.write(
                    toFile: dir + "/tree-\(name.replacingOccurrences(of: " ", with: "_")).txt", atomically: true, encoding: .utf8)
            }
        }
        app?.terminate()
        if let stateDir { try? FileManager.default.removeItem(at: stateDir) }
    }

    private func element(_ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    private func appears(_ e: XCUIElement, _ what: String, timeout: TimeInterval = 15, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(e.waitForExistence(timeout: timeout), "\(what) did not appear", file: file, line: line)
    }

    private func disappears(_ e: XCUIElement, _ what: String, timeout: TimeInterval = 10, file: StaticString = #filePath, line: UInt = #line) {
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: e)
        XCTAssertEqual(XCTWaiter().wait(for: [gone], timeout: timeout), .completed, "\(what) did not go away", file: file, line: line)
    }

    private func openPopover(file: StaticString = #filePath, line: UInt = #line) {
        let item = app.statusItems.firstMatch.exists ? app.statusItems.firstMatch : app.menuBars.statusItems.firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 15), "menu-bar status item not found", file: file, line: line)
        item.click()
        appears(element("glomeris.popover.header"), "popover header", file: file, line: line)
    }

    private func refresh() {
        let button = element("glomeris.candidates.refresh")
        appears(button, "Refresh button")
        button.click()
    }

    /// A window-level screenshot only, attached to the result bundle and written
    /// to GLOMERIS_UITEST_ARTIFACT_DIR when set. Never the full desktop.
    ///
    /// Opt-in (GLOMERIS_UITEST_SCREENSHOTS=1) because capturing a window needs
    /// the Screen Recording permission for the test runner, which is a separate
    /// one-time owner grant from Accessibility; without it the capture API
    /// reports a test failure that has nothing to do with the app.
    private func shoot(_ window: XCUIElement, _ name: String) {
        guard ProcessInfo.processInfo.environment["GLOMERIS_UITEST_SCREENSHOTS"] == "1", window.exists else { return }
        let shot = window.screenshot()
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = ProcessInfo.processInfo.environment["GLOMERIS_UITEST_ARTIFACT_DIR"] {
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            try? shot.pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
        }
    }

    private func closeBrowserWindow() {
        let window = app.windows.containing(.any, identifier: "glomeris.browser.root").firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 10), "no candidates window to close")
        let close = window.buttons[XCUIIdentifierCloseWindow]
        close.click()
        // With uncommitted input-method text in the search field, the first click
        // can be spent ending that composition. Report it rather than hide it.
        if element("glomeris.browser.root").waitForNonExistence(timeout: 3) == false {
            print("NOTE closeBrowserWindow: window still open after the first click; clicking again")
            window.buttons[XCUIIdentifierCloseWindow].click()
        }
    }

    /// True when the current keyboard input source types ASCII. A synthetic
    /// Cmd+<letter> is translated through the current input source; under a
    /// non-ASCII one (this machine uses Traditional Chinese Zhuyin) it never
    /// reaches the app as a key equivalent, which is a harness limit and not
    /// evidence about the app (measured in HORO-2176).
    private var inputSourceIsASCIICapable: Bool {
        let source = TISCopyCurrentKeyboardInputSource().takeRetainedValue()
        guard let raw = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsASCIICapable) else { return false }
        return CFBooleanGetValue(Unmanaged<CFBoolean>.fromOpaque(raw).takeUnretainedValue())
    }

    /// Static text found by its displayed string (SwiftUI exposes it as the value).
    private func staticText(value: String) -> XCUIElement {
        app.staticTexts.matching(NSPredicate(format: "value == %@", value)).firstMatch
    }

    /// Click an element even when AX reports it "not hittable" (measured on a
    /// window placed on a secondary display): fall back to its centre point.
    private func tap(_ e: XCUIElement) {
        if e.isHittable { e.click() } else { e.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).click() }
    }

    /// SwiftUI `Text` exposes its string as the element's value, not its label.
    private func text(_ e: XCUIElement) -> String { (e.value as? String) ?? e.label }

    private func popoverWindow() -> XCUIElement {
        // The popover is exposed as a Dialog, not a Window (measured).
        app.dialogs.containing(.any, identifier: "glomeris.popover.header").firstMatch
    }

    private func invocations() -> [String] {
        let log = stateDir.appendingPathComponent("invocations.log")
        return ((try? String(contentsOf: log, encoding: .utf8)) ?? "")
            .split(separator: "\n").map(String.init)
    }

    // MARK: - Golden journey (220 candidates: the size observed in the rc.1 run)

    func testGoldenJourney() throws {
        try launch(count: 220)

        // 2-3. Popover opens; the DEV-QA indicator is visible; compact summary.
        openPopover()
        appears(element("glomeris.qa.banner"), "DEV-QA synthetic-data banner")
        refresh()
        let summary = element("glomeris.candidates.summary")
        appears(summary, "popover summary")
        XCTAssertEqual(text(summary), expectedSummary(220))
        shoot(popoverWindow(), "01-popover-220")

        // 4. A fixed five-row preview, not 220 rows.
        for i in 0..<5 { XCTAssertTrue(element("glomeris.candidates.preview.row.\(i)").exists, "preview row \(i)") }
        XCTAssertFalse(element("glomeris.candidates.preview.row.5").exists, "preview must stop at five rows")

        // 5-6. View all N opens the dedicated window.
        let viewAll = element("glomeris.candidates.viewAll")
        appears(viewAll, "View all button")
        XCTAssertEqual(viewAll.label, "View all 220 candidates in a window")
        viewAll.click()
        let root = element("glomeris.browser.root")
        appears(root, "candidates window")
        appears(element("glomeris.qa.banner"), "banner in window")
        XCTAssertEqual(text(element("glomeris.browser.summary")), expectedSummary(220))
        appears(element("glomeris.browser.placeholder.none"), "'Select a candidate' placeholder")
        shoot(app.windows.containing(.any, identifier: "glomeris.browser.root").firstMatch, "02-window-220")

        // 7. Arrow keys move the selection and open the detail pane.
        let list = element("glomeris.browser.list")
        appears(list, "candidate list")
        // Focus the list by clicking near its top (that selects a row, so the
        // placeholder is checked before this click), then use only the keyboard.
        list.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.08)).click()
        app.typeKey(.downArrow, modifierFlags: [])
        appears(element("glomeris.browser.detail"), "detail pane after Down arrow")
        XCTAssertFalse(element("glomeris.browser.placeholder.none").exists)
        app.typeKey(.downArrow, modifierFlags: [])
        app.typeKey(.upArrow, modifierFlags: [])
        XCTAssertTrue(element("glomeris.browser.detail").exists, "selection survives Down/Up")

        // 8. Search narrows to one row; the count says so.
        let search = app.searchFields.firstMatch
        appears(search, "search field")
        tap(search)
        search.typeText("00001")
        // Return commits any input-method composition (this session's input
        // source types full-width digits as marked text, which the search
        // binding does not see until committed) and is a no-op otherwise.
        app.typeKey(.return, modifierFlags: [])
        appears(staticText(value: "Showing 1 of 220"), "'Showing 1 of 220'")
        let id1 = syntheticId(1)
        let row = element("glomeris.browser.row.\(id1)")
        appears(row, "searched row")

        // 9. Inspect item details and its risk status (index 1 is an ASK candidate).
        tap(row)
        appears(element("glomeris.browser.detail"), "detail for the searched row")
        shoot(app.windows.containing(.any, identifier: "glomeris.browser.root").firstMatch, "03-window-detail")

        // 10. Remove that item from the synthetic results, refresh from the
        //     popover, and the window must say "No longer shown" — it must not
        //     silently select a neighbour.
        try id1.write(to: stateDir.appendingPathComponent("dropped.txt"), atomically: true, encoding: .utf8)
        openPopover()
        refresh()
        appears(element("glomeris.browser.placeholder.unavailable"), "'No longer shown' placeholder")
        XCTAssertFalse(element("glomeris.browser.row.\(id1)").exists)

        // 11. The window closes (via its close button; the Cmd+W shortcut has its
        //     own test because synthetic key events cannot reach it under a
        //     non-ASCII input source), and "View all" opens it again.
        closeBrowserWindow()
        disappears(element("glomeris.browser.root"), "candidates window after closing")
        openPopover()
        element("glomeris.candidates.viewAll").click()
        appears(element("glomeris.browser.root"), "candidates window reopened")
        closeBrowserWindow()
        disappears(element("glomeris.browser.root"), "candidates window after closing again")

        // 12. Nothing acted: the stand-in CLI saw only read-only commands.
        let log = invocations()
        XCTAssertTrue(log.contains("detect"), "detect never ran: \(log)")
        XCTAssertTrue(log.contains("explain"), "explain never ran: \(log)")
        let mutating: Set<String> = ["execute", "apply", "sweep", "enable", "revoke", "set", "create", "produce", "seal", "run"]
        let refused = log.filter { $0.hasPrefix("REFUSED ") }.map { String($0.dropFirst("REFUSED ".count)) }
        XCTAssertTrue(refused.allSatisfy { !mutating.contains($0) }, "a mutating command was attempted: \(refused)")
    }

    // MARK: - Cmd+W from each place focus can be

    private func openWindow(count: Int) throws {
        try launch(count: count)
        openPopover()
        refresh()
        let viewAll = element("glomeris.candidates.viewAll")
        appears(viewAll, "View all")
        viewAll.click()
        appears(element("glomeris.browser.root"), "candidates window")
    }

    func testCmdWClosesTheWindow() throws {
        try XCTSkipUnless(
            inputSourceIsASCIICapable,
            "Cmd+W cannot be synthesised under this non-ASCII input source; verify the shortcut by hand")
        try openWindow(count: 25)
        element("glomeris.browser.list").coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.08)).click()
        app.typeKey("w", modifierFlags: .command)
        disappears(element("glomeris.browser.root"), "window after Cmd+W")
    }

    // MARK: - Result-set sizes

    private func checkPopoverAtScale(_ n: Int) throws {
        try launch(count: n)
        openPopover()
        refresh()
        if n == 0 {
            XCTAssertFalse(element("glomeris.candidates.summary").exists)
            XCTAssertFalse(element("glomeris.candidates.viewAll").exists)
            return
        }
        let summary = element("glomeris.candidates.summary")
        appears(summary, "summary at n=\(n)", timeout: 60)
        XCTAssertEqual(text(summary), expectedSummary(n), "n=\(n)")
        for i in 0..<min(n, 5) { XCTAssertTrue(element("glomeris.candidates.preview.row.\(i)").exists, "n=\(n) row \(i)") }
        XCTAssertFalse(element("glomeris.candidates.preview.row.5").exists, "n=\(n) preview exceeds five rows")
        XCTAssertEqual(element("glomeris.candidates.viewAll").exists, n > 5, "n=\(n) View all presence")
        // The popover does not grow with the result set.
        let height = popoverWindow().frame.height
        XCTAssertGreaterThan(height, 0)
        XCTAssertLessThan(height, 1_000, "n=\(n): popover is \(height)pt tall")
    }

    func testScale0() throws { try checkPopoverAtScale(0) }
    func testScale1() throws { try checkPopoverAtScale(1) }
    func testScale5() throws { try checkPopoverAtScale(5) }
    func testScale25() throws { try checkPopoverAtScale(25) }
    func testScale1000() throws { try checkPopoverAtScale(1_000) }
    func testScale5000() throws { try checkPopoverAtScale(5_000) }

    func testPopoverHeightDoesNotGrowBetween25And5000() throws {
        try launch(count: 25)
        openPopover()
        refresh()
        appears(element("glomeris.candidates.summary"), "summary at 25")
        let small = popoverWindow().frame.height
        app.terminate()
        try launch(count: 5_000)
        openPopover()
        refresh()
        appears(element("glomeris.candidates.summary"), "summary at 5000", timeout: 60)
        let large = popoverWindow().frame.height
        // One wrapped summary line at four digits is the only permitted difference.
        XCTAssertLessThanOrEqual(abs(large - small), 40, "popover grew from \(small) to \(large)")
    }

    // MARK: - Accessibility

    func testAccessibleLabels() throws {
        try launch(count: 25)
        openPopover()
        refresh()
        let viewAll = element("glomeris.candidates.viewAll")
        appears(viewAll, "View all")
        XCTAssertEqual(viewAll.label, "View all 25 candidates in a window")
        for i in 0..<5 {
            let rowLabel = element("glomeris.candidates.preview.row.\(i)").label
            XCTAssertFalse(rowLabel.isEmpty, "preview row \(i) has no accessibility label")
        }
        viewAll.click()
        appears(element("glomeris.browser.root"), "window")
        XCTAssertEqual(element("glomeris.browser.list").label, "Candidates, 25")
        // Every list row carries a spoken label.
        let id0 = syntheticId(0)
        XCTAssertFalse(element("glomeris.browser.row.\(id0)").label.isEmpty)
    }

    /// Apple's accessibility audit, element descriptions only (contrast and
    /// hit-region audits include system chrome the app does not control). The
    /// audit captures screenshots, so it needs Screen Recording for the runner;
    /// it is skipped, never reported as passed, unless GLOMERIS_UITEST_SCREENSHOTS=1.
    func testSystemAccessibilityAuditOfCandidatesWindow() throws {
        try XCTSkipUnless(
            ProcessInfo.processInfo.environment["GLOMERIS_UITEST_SCREENSHOTS"] == "1",
            "needs Screen Recording permission for the test runner")
        try launch(count: 25)
        openPopover()
        refresh()
        element("glomeris.candidates.viewAll").click()
        appears(element("glomeris.browser.root"), "window")
        if #available(macOS 14.0, *) {
            try app.performAccessibilityAudit(for: .sufficientElementDescription)
        }
    }
}
