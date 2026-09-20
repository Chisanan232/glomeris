//
//  GlomerisExecutableLocatorTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1295: executable resolution tests.
//
//  Every case injects the filesystem predicate rather than creating real
//  files, so the assertions are about resolution ORDER and are unaffected by
//  whether the host running the suite happens to have a `glomeris` installed
//  — which is the whole class of bug being fixed here, and would otherwise
//  make a test pass on the author's machine and fail on CI (or vice versa).
//

import XCTest

final class GlomerisExecutableLocatorTests: XCTestCase {
    private let bundled = URL(fileURLWithPath: "/Applications/GlomerisMenuBar.app/Contents/MacOS/glomeris")

    /// A locator where only the listed absolute paths are executable.
    private func locator(
        bundled bundledURL: URL? = nil,
        path pathVariable: String? = nil,
        knownInstallDirectories: [String] = GlomerisExecutableLocator.knownInstallDirectories,
        executable: Set<String>
    ) -> GlomerisExecutableLocator {
        GlomerisExecutableLocator(
            bundledExecutableURL: bundledURL,
            pathVariable: pathVariable,
            knownInstallDirectories: knownInstallDirectories,
            isExecutableFile: { executable.contains($0.path) }
        )
    }

    // MARK: - The reported defect

    /// HORO-1295's actual report: a documented `brew install` on Apple
    /// Silicon, and the GUI-launched app's minimal PATH. This exact
    /// combination is what the old hardcoded `/usr/local/bin/glomeris`
    /// default could not resolve.
    func testResolvesHomebrewOnAppleSiliconWithAGUIAppPath() {
        let located = locator(
            path: "/usr/bin:/bin:/usr/sbin:/sbin",
            executable: ["/opt/homebrew/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/opt/homebrew/bin/glomeris")
        XCTAssertEqual(located?.source, .knownInstallDirectory("/opt/homebrew/bin"))
    }

    /// The Intel case the old hardcoded path got right by coincidence, since
    /// there the Homebrew prefix *is* `/usr/local`. It must keep working.
    func testResolvesHomebrewOnIntelWithAGUIAppPath() {
        let located = locator(
            path: "/usr/bin:/bin:/usr/sbin:/sbin",
            executable: ["/usr/local/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/usr/local/bin/glomeris")
        XCTAssertEqual(located?.source, .knownInstallDirectory("/usr/local/bin"))
    }

    // MARK: - Precedence

    func testBundledExecutableWinsOverEverythingElse() {
        let located = locator(
            bundled: bundled,
            path: "/opt/homebrew/bin",
            executable: [bundled.path, "/opt/homebrew/bin/glomeris", "/usr/local/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url, bundled)
        XCTAssertEqual(located?.source, .bundled)
    }

    /// A bundled URL that is present but not executable must not shadow a
    /// working CLI elsewhere. The release workflow ad-hoc signs the bundle
    /// after copying the CLI in, so a bundle whose embedded binary lost its
    /// executable bit is a real state to survive rather than a hypothetical.
    func testNonExecutableBundledBinaryFallsThrough() {
        let located = locator(
            bundled: bundled,
            path: "/opt/homebrew/bin",
            executable: ["/opt/homebrew/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/opt/homebrew/bin/glomeris")
        XCTAssertEqual(located?.source, .pathEntry("/opt/homebrew/bin"))
    }

    func testPathWinsOverKnownInstallDirectories() {
        let located = locator(
            path: "/opt/custom/bin",
            executable: ["/opt/custom/bin/glomeris", "/usr/local/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/opt/custom/bin/glomeris")
        XCTAssertEqual(located?.source, .pathEntry("/opt/custom/bin"))
    }

    func testEarlierPathEntryWinsOverLaterOne() {
        let located = locator(
            path: "/first/bin:/second/bin",
            executable: ["/first/bin/glomeris", "/second/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/first/bin/glomeris")
    }

    func testHomebrewPrefixIsPreferredOverLegacyPath() {
        let located = locator(
            executable: ["/opt/homebrew/bin/glomeris", "/usr/local/bin/glomeris"]
        ).locate()

        XCTAssertEqual(located?.url.path, "/opt/homebrew/bin/glomeris")
    }

    // MARK: - Nothing found

    func testReturnsNilWhenNoExecutableExistsAnywhere() {
        let located = locator(
            bundled: bundled,
            path: "/usr/bin:/bin:/usr/sbin:/sbin",
            executable: []
        ).locate()

        XCTAssertNil(located)
    }

    func testSearchedLocationsNamesEveryPlaceItLooked() {
        let searched = locator(path: "/usr/bin:/bin", executable: []).searchedLocations

        XCTAssertEqual(searched, ["the app bundle", "PATH", "/opt/homebrew/bin", "/usr/local/bin"])
    }

    /// With no PATH set there is no PATH to blame, and saying otherwise would
    /// send someone to inspect a variable that played no part.
    func testSearchedLocationsOmitsPathWhenThereIsNone() {
        let searched = locator(path: nil, executable: []).searchedLocations

        XCTAssertEqual(searched, ["the app bundle", "/opt/homebrew/bin", "/usr/local/bin"])
    }

    // MARK: - PATH parsing

    /// A relative entry must never be resolved: it would let the contents of
    /// whatever directory the app is running in decide which binary receives
    /// all of Glomeris's policy and execution authority.
    func testRelativePathEntriesAreIgnored() {
        let candidate = locator(
            path: ".:relative/bin:/absolute/bin",
            executable: ["./glomeris", "relative/bin/glomeris", "/absolute/bin/glomeris"]
        )

        XCTAssertEqual(candidate.pathDirectories, ["/absolute/bin"])
        XCTAssertEqual(candidate.locate()?.url.path, "/absolute/bin/glomeris")
    }

    /// The empty entry in `PATH=/usr/bin::/bin` is a shell's way of writing
    /// "the current directory", so it is dropped for the same reason.
    func testEmptyPathEntryIsIgnored() {
        XCTAssertEqual(
            locator(path: "/usr/bin::/bin", executable: []).pathDirectories,
            ["/usr/bin", "/bin"]
        )
    }

    func testNilPathYieldsNoDirectories() {
        XCTAssertEqual(locator(path: nil, executable: []).pathDirectories, [])
    }
}
