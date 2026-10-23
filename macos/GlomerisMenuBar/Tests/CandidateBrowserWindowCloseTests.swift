//
//  CandidateBrowserWindowCloseTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2176: Cmd+W on the candidates window must not depend on the keyboard
//  input source. Under a non-ASCII source (Zhuyin, Pinyin, Cyrillic, ...) the
//  event's `charactersIgnoringModifiers` can be that script's character rather
//  than "w", so matching on the character alone silently loses the shortcut.
//

import AppKit
import XCTest

@MainActor
final class CandidateBrowserWindowCloseTests: XCTestCase {
    private final class Recorder: NSObject, NSWindowDelegate {
        var closeRequests = 0
        func windowShouldClose(_ sender: NSWindow) -> Bool {
            closeRequests += 1
            return false  // observe the request without actually closing
        }
    }

    private func keyDown(
        _ characters: String, keyCode: UInt16, flags: NSEvent.ModifierFlags = .command
    ) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0,
                context: nil, characters: characters, charactersIgnoringModifiers: characters,
                isARepeat: false, keyCode: keyCode))
    }

    private func closeRequests(for event: NSEvent) -> (handled: Bool, requests: Int) {
        let recorder = Recorder()
        let window = CandidateBrowserNSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
            styleMask: [.titled, .closable], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        window.delegate = recorder
        let handled = window.performKeyEquivalent(with: event)
        return (handled, recorder.closeRequests)
    }

    func testCmdWWithAnAsciiLayoutRequestsClose() throws {
        let result = closeRequests(for: try keyDown("w", keyCode: 13))
        XCTAssertTrue(result.handled)
        XCTAssertEqual(result.requests, 1)
    }

    func testCmdWUnderZhuyinStillRequestsClose() throws {
        // Key W produces ㄊ on the Zhuyin layout; the physical key is still W.
        let result = closeRequests(for: try keyDown("ㄊ", keyCode: 13))
        XCTAssertTrue(result.handled)
        XCTAssertEqual(result.requests, 1)
    }

    func testCmdWUnderCyrillicStillRequestsClose() throws {
        let result = closeRequests(for: try keyDown("ц", keyCode: 13))
        XCTAssertTrue(result.handled)
        XCTAssertEqual(result.requests, 1)
    }

    func testOtherCommandShortcutsAreNotClaimed() throws {
        // Cmd+Q (key code 12) and Cmd+C (key code 8) belong to the system and the field editor.
        for (character, code) in [("q", UInt16(12)), ("c", UInt16(8)), ("ㄆ", UInt16(12))] {
            let result = closeRequests(for: try keyDown(character, keyCode: code))
            XCTAssertEqual(result.requests, 0, "\(character) must not close the window")
        }
    }

    func testPlainWAndModifiedCmdWAreNotClaimed() throws {
        XCTAssertEqual(closeRequests(for: try keyDown("w", keyCode: 13, flags: [])).requests, 0)
        XCTAssertEqual(closeRequests(for: try keyDown("w", keyCode: 13, flags: [.command, .shift])).requests, 0)
        XCTAssertEqual(closeRequests(for: try keyDown("w", keyCode: 13, flags: [.command, .option])).requests, 0)
    }

    func testAnAsciiLayoutWhereWIsElsewhereIsNotConfusedWithTheKeyCode() throws {
        // Dvorak: the physical key at code 13 types ",", and "w" lives at code 43.
        XCTAssertEqual(closeRequests(for: try keyDown(",", keyCode: 13)).requests, 0)
        XCTAssertEqual(closeRequests(for: try keyDown("w", keyCode: 43)).requests, 1)
    }
}
