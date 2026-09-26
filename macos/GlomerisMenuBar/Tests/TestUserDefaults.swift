//
//  TestUserDefaults.swift
//  GlomerisMenuBarTests
//
//  HORO-1486: the one place in this target that produces a `UserDefaults` for
//  a test to write through.
//

import Foundation
import XCTest

/// A `UserDefaults` that keeps everything in memory and writes no file.
///
/// **Do not replace this with `UserDefaults(suiteName:)` and a teardown.** That
/// is what this target did before HORO-1486, and it had accumulated 1,892
/// preference files on one developer's machine. Three measurements, in the
/// order they ruled options out:
///
/// 1. `removePersistentDomain(forName:)` empties the domain but does not remove
///    the file. A suite written once and torn down leaves its plist behind.
/// 2. Unlinking the plist from inside the test process does not stick either:
///    `cfprefsd` still holds the domain and rewrites an empty plist when the
///    process disconnects. Measured over 40 suites — 40 of 40 reappeared.
/// 3. Unlinking, then polling for half a second re-unlinking whenever the file
///    reappeared, still lost 36 of 40. There is no in-process teardown that
///    reliably removes the file, because the write that recreates it happens
///    after the last line of teardown has run.
///
/// So the fix is not a better teardown, it is not creating the file. A test
/// that needs somewhere to put a value needs storage, not a plist, and this
/// provides exactly that.
///
/// The typed accessors (`string(forKey:)`, `stringArray(forKey:)`, …) are not
/// overridden because `NSUserDefaults` routes them through `object(forKey:)`,
/// which is. That is asserted rather than assumed — see
/// `TestUserDefaultsTests`, which drives the same accessors production code
/// uses and would fail if a future Foundation stopped routing them.
final class InMemoryUserDefaults: UserDefaults {
    private var storage: [String: Any] = [:]

    override func object(forKey defaultName: String) -> Any? {
        storage[defaultName]
    }

    override func set(_ value: Any?, forKey defaultName: String) {
        if let value {
            storage[defaultName] = value
        } else {
            storage.removeValue(forKey: defaultName)
        }
    }

    override func removeObject(forKey defaultName: String) {
        storage.removeValue(forKey: defaultName)
    }

    /// Everything written through this object, for a test that has to look at
    /// the whole domain rather than at one key it already knows the name of.
    ///
    /// Deliberately not `dictionaryRepresentation()`: on a real
    /// `UserDefaults` that also returns the global and argument domains, so a
    /// test reading it could not distinguish "this store persisted the value"
    /// from "something else in the search list has a key of that name."
    var everythingStored: [String: Any] { storage }
}

/// The only sanctioned ways for a test in this target to obtain a
/// `UserDefaults`, enforced by
/// `scripts/check-tests-do-not-open-user-defaults-suites.sh`.
enum TestUserDefaults {
    /// Storage for a test, backed by no file. The default choice; use it
    /// unless the subject of the test is a real preference domain.
    static func inMemory() -> InMemoryUserDefaults {
        InMemoryUserDefaults()
    }

    /// The one real, file-backed suite this target opens, for the single test
    /// whose subject *is* whether a suite named beneath the bundle identifier
    /// is a separate domain from `.standard` — a question an in-memory double
    /// cannot answer, because it would separate the two by construction and
    /// pass however `CFPreferences` behaved.
    ///
    /// The name is fixed rather than made unique per run, which is the whole
    /// point: it occupies one file forever instead of one more file every time
    /// the suite runs. Contents are still cleared in teardown, so no run
    /// inherits another's values.
    static func realSuiteForDomainSeparation(
        _ testCase: XCTestCase
    ) -> UserDefaults? {
        let name = realSuiteName
        guard let suite = UserDefaults(suiteName: name) else { return nil }
        testCase.addTeardownBlock {
            suite.removePersistentDomain(forName: name)
        }
        return suite
    }

    /// Exposed so the guard script and the test that reads it agree on one
    /// spelling, and so the file this target does leave behind is named in
    /// source rather than only observable on a machine that has run it.
    static let realSuiteName = "\(BundleIdentity.current).h1486-domain-separation-probe"
}
