//
//  BundleIdentityTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1456: the names this app files persistent state under must come from
//  the running bundle, not from a literal that happens to match it.
//
//  What a test process can and cannot establish here is worth stating up
//  front, because one of the two facts behind this change is not assertable
//  from inside this target.
//
//  ASSERTABLE, and asserted below: that the three names are derived; that in a
//  process which is not the app they are therefore not the app's; that the
//  fallback rule does what it says for both kinds of input; and that the two
//  preference stores default to the domain they claim to.
//
//  NOT ASSERTABLE here: that `UserDefaults(suiteName:)` returns nil when handed
//  the calling process's *own* bundle identifier — the Foundation behaviour
//  that makes "the app was already using .standard, so nothing needs migrating"
//  true. Establishing it needs a process whose own identifier is the one being
//  passed, and this process reports `com.apple.dt.xctest.tool` while the suite
//  names at issue were the app's. It was verified against a purpose-built
//  bundled executable instead, and the output is recorded on the ticket. A test
//  that self-skipped here would be the HORO-1253 failure mode: a gate that
//  never runs reading as a gate that passed.
//

import XCTest

final class BundleIdentityTests: XCTestCase {
    /// The identifier the shipped app is built with — `project.yml`'s
    /// `PRODUCT_BUNDLE_IDENTIFIER` for the `GlomerisMenuBar` target.
    ///
    /// Written out here on purpose. This is the one file that should contain
    /// it, because the assertions below are about *not* being it.
    private static let releaseBundleIdentifier = "dev.glomeris.GlomerisMenuBar"

    // MARK: - The identity itself

    func testTheIdentityIsReadFromTheRunningBundle() {
        XCTAssertEqual(
            BundleIdentity.current, Bundle.main.bundleIdentifier,
            "the identity must be read from the running bundle, not constructed")
    }

    /// The branch the app never takes, driven through the seam. Left as a
    /// runtime `if` it would be dead code in every process this target can be
    /// loaded into, and a fallback nothing exercises is a fallback nobody knows
    /// the value of.
    func testAnExecutableDeclaringNoIdentifierGetsTheFallback() {
        XCTAssertEqual(
            BundleIdentity.identity(declaredBy: nil), BundleIdentity.unidentifiedProcess)
    }

    func testADeclaredIdentifierIsUsedVerbatim() {
        XCTAssertEqual(
            BundleIdentity.identity(declaredBy: "dev.glomeris.SomeOtherChannel"),
            "dev.glomeris.SomeOtherChannel",
            "a declared identifier must be used as-is, not rewritten towards the release one")
    }

    func testTheFallbackIsNotTheReleaseIdentifier() {
        XCTAssertNotEqual(
            BundleIdentity.identity(declaredBy: nil), Self.releaseBundleIdentifier,
            """
            a process with no bundle identifier of its own must not be given the \
            release app's: it would be handed the user's real keychain items, \
            which is the defect this ticket removed
            """)
    }

    /// The isolation property, asserted in the only process that can assert it:
    /// a build that is not the app must not resolve to the app's identity.
    ///
    /// This test fails if the test target is ever given the app as a test host,
    /// because `Bundle.main` would then be the app bundle and every default-
    /// constructed store in the suite would be pointed at the user's real
    /// keychain items and real preferences. That is a decision to take
    /// deliberately, not to discover from a flaky keychain prompt, so failing
    /// here is the intended behaviour rather than a brittle assertion.
    func testAProcessThatIsNotTheAppDoesNotResolveToTheAppsIdentity() {
        XCTAssertNotEqual(
            BundleIdentity.current, Self.releaseBundleIdentifier,
            "this process is not the shipped app, so it must not claim the app's identity")
    }

    // MARK: - What the three names derive from it

    func testTheKeychainServiceIsTheRunningIdentity() {
        XCTAssertEqual(
            KeychainCredentialStore.service, BundleIdentity.current,
            "every stored item must be filed under the running bundle's identifier")
    }

    /// The consequence that matters: a test run cannot reach the BYOK API key
    /// the user stored through the shipped app, because it asks the keychain
    /// for a different service.
    func testATestRunCannotReachTheReleaseAppsKeychainItems() {
        XCTAssertNotEqual(
            KeychainCredentialStore.service, Self.releaseBundleIdentifier,
            "a default-constructed store in a test must not query the release app's service")
    }

    func testTheDefaultProjectRootsStoreReadsStandardDefaults() {
        let key = "projectRoots"
        let restore = restoringStandardValue(forKey: key)
        defer { restore() }

        let planted = ["/private/tmp/h1456-project-root-\(UUID().uuidString)"]
        UserDefaults.standard.set(planted, forKey: key)

        XCTAssertEqual(
            ProjectRootsStore().roots, planted,
            """
            a store constructed with no explicit defaults must read \
            UserDefaults.standard — which for a bundled app is the domain named \
            by its bundle identifier
            """)
    }

    func testTheDefaultLlmSettingsStoreReadsStandardDefaults() {
        let key = "llmBaseUrl"
        let restore = restoringStandardValue(forKey: key)
        defer { restore() }

        let planted = "https://example.invalid/h1456/\(UUID().uuidString)"
        UserDefaults.standard.set(planted, forKey: key)

        // An in-memory credential store so nothing here reaches the keychain:
        // the endpoint is the subject, and a real SecItemCopyMatching could
        // raise an authorisation prompt in the middle of a test run.
        let store = GlomerisLlmSettingsStore(credentials: InMemoryCredentialStore())

        XCTAssertEqual(
            store.endpoint, planted,
            "a store constructed with no explicit defaults must read UserDefaults.standard")
    }

    // MARK: - Why not a suite suffixed beneath the identifier

    /// The other option the ticket names, shown to be the one that would have
    /// cost a migration: a suite named beneath the bundle identifier is a
    /// genuinely separate domain, so moving the existing keys into it would
    /// have orphaned every value already stored by a shipped app.
    ///
    /// `.standard` costs nothing because it is where those values already are.
    func testASuiteBeneathTheIdentityWouldBeASeparateDomain() {
        // The one real, file-backed suite this target opens. An in-memory
        // double would separate the two domains by construction and pass
        // however `CFPreferences` behaved, which is the opposite of what this
        // asserts. Its name is fixed rather than unique per run, so it
        // occupies one file rather than one more on every run (HORO-1486).
        guard let suite = TestUserDefaults.realSuiteForDomainSeparation(self) else {
            return XCTFail("a suite name below the bundle identifier must be usable")
        }

        let key = "h1456ProbeKey"
        let restore = restoringStandardValue(forKey: key)
        defer { restore() }

        suite.set("written-through-the-suffixed-suite", forKey: key)

        XCTAssertEqual(suite.string(forKey: key), "written-through-the-suffixed-suite")
        XCTAssertNil(
            UserDefaults.standard.string(forKey: key),
            """
            a suffixed suite is a separate domain, so adopting one would have \
            left every already-stored value unreachable
            """)
    }

    // MARK: - Helpers

    /// Captures whatever `UserDefaults.standard` currently holds for `key` and
    /// returns the closure that puts it back, so a test can plant a value in a
    /// shared domain without deciding what was there before was disposable.
    private func restoringStandardValue(forKey key: String) -> () -> Void {
        let previous = UserDefaults.standard.object(forKey: key)
        return {
            if let previous {
                UserDefaults.standard.set(previous, forKey: key)
            } else {
                UserDefaults.standard.removeObject(forKey: key)
            }
        }
    }
}
