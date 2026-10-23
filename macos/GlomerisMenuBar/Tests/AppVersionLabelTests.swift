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
        XCTAssertTrue(source.contains("AppVersionLabel.running"))
    }
}
