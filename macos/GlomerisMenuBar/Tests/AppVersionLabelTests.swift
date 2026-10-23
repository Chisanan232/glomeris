//
//  AppVersionLabelTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2170.
//

import XCTest

final class AppVersionLabelTests: XCTestCase {
    func testReadsTheMarketingVersion() {
        XCTAssertEqual(
            AppVersionLabel.version(from: ["CFBundleShortVersionString": "0.3.0-rc.1"]), "0.3.0-rc.1")
    }

    func testTrimsSurroundingWhitespace() {
        XCTAssertEqual(AppVersionLabel.version(from: ["CFBundleShortVersionString": " 1.2.3\n"]), "1.2.3")
    }

    func testMissingBlankOrUnexpandedVersionIsNil() {
        XCTAssertNil(AppVersionLabel.version(from: nil))
        XCTAssertNil(AppVersionLabel.version(from: [:]))
        XCTAssertNil(AppVersionLabel.version(from: ["CFBundleShortVersionString": "  "]))
        XCTAssertNil(AppVersionLabel.version(from: ["CFBundleShortVersionString": "$(MARKETING_VERSION)"]))
        XCTAssertNil(AppVersionLabel.version(from: ["CFBundleShortVersionString": 3]))
    }

    func testSpokenLabelNamesTheVersionOnlyWhenKnown() {
        XCTAssertEqual(
            AppVersionLabel.spokenLabel(title: "Glomeris", version: "0.3.0-rc.1"),
            "Glomeris, version 0.3.0-rc.1")
        XCTAssertEqual(AppVersionLabel.spokenLabel(title: "Glomeris", version: nil), "Glomeris")
    }

    func testHeaderShowsTheVersionThroughThisType() throws {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/GlomerisPopoverView.swift")
        let source = try String(contentsOf: url, encoding: .utf8)
        // Code lines only, so a mention in a comment cannot satisfy this.
        let code = source.split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
        XCTAssertTrue(code.contains("if let version = AppVersionLabel.running"))
        XCTAssertTrue(code.contains("AppVersionLabel.spokenLabel(title: MenuBarAppearance.title"))
    }
}
