//
//  TestUserDefaultsTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1486: the test double the rest of this target now writes through, under
//  test itself.
//
//  Worth saying why this file exists, because a helper's tests are often
//  ceremony. `InMemoryUserDefaults` overrides three methods and relies on
//  Foundation routing the typed accessors through one of them. Production code
//  calls `string(forKey:)` and `stringArray(forKey:)`, never `object(forKey:)`
//  directly — so if that routing ever stopped, every store in this suite would
//  read nil for values a test had just written, and the failures would surface
//  as a dozen unrelated tests going red with no obvious cause. This pins the
//  assumption in one place, next to the code that depends on it.
//
//  What is NOT asserted here: that a run of this target leaves no new file in
//  the preferences directory. That is a property of the whole suite, not of one
//  object, and a before/after count inside a single test would race cfprefsd —
//  which writes after the process that asked it to disconnects. It is
//  demonstrated by counting the directory across two consecutive full runs, and
//  the numbers are recorded on the ticket.
//

import XCTest

final class TestUserDefaultsTests: XCTestCase {
    // MARK: - The routing the three overrides rely on

    /// `string(forKey:)` is what `GlomerisLlmSettingsStore` reads the endpoint
    /// and model with, and it is not overridden.
    func testAStringWrittenThroughSetIsReadBackByStringForKey() {
        let defaults = TestUserDefaults.inMemory()

        defaults.set("https://example.invalid/h1486", forKey: "llmBaseUrl")

        XCTAssertEqual(
            defaults.string(forKey: "llmBaseUrl"), "https://example.invalid/h1486",
            """
            string(forKey:) must reach the overridden object(forKey:) — if \
            Foundation stops routing it there, every settings store in this \
            target silently reads nil
            """)
    }

    /// `stringArray(forKey:)` is what `ProjectRootsStore` reads the roots with.
    func testAStringArrayWrittenThroughSetIsReadBackByStringArrayForKey() {
        let defaults = TestUserDefaults.inMemory()
        let roots = ["/private/tmp/h1486-a", "/private/tmp/h1486-b"]

        defaults.set(roots, forKey: "projectRoots")

        XCTAssertEqual(
            defaults.stringArray(forKey: "projectRoots"), roots,
            "stringArray(forKey:) must reach the overridden object(forKey:)")
    }

    /// Not called by production today. Asserted anyway because the next caller
    /// to reach for one should find out from a failing test in this file rather
    /// than from a value that reads as `false` or `0` for no visible reason.
    func testBoolAndIntegerAccessorsAlsoRouteThroughTheOverride() {
        let defaults = TestUserDefaults.inMemory()

        defaults.set(true, forKey: "aFlag")
        defaults.set(7, forKey: "aCount")

        XCTAssertTrue(defaults.bool(forKey: "aFlag"))
        XCTAssertEqual(defaults.integer(forKey: "aCount"), 7)
    }

    // MARK: - Removal, both spellings

    func testRemoveObjectRemovesTheKeyRatherThanNillingIt() {
        let defaults = TestUserDefaults.inMemory()
        defaults.set("value", forKey: "aKey")

        defaults.removeObject(forKey: "aKey")

        XCTAssertNil(defaults.string(forKey: "aKey"))
        XCTAssertFalse(
            defaults.everythingStored.keys.contains("aKey"),
            """
            the key must be gone, not present holding nil: a test asserting \
            "the store wrote nothing under this name" would otherwise pass on a \
            store that wrote and then blanked it
            """)
    }

    /// `set(nil, forKey:)` removes on a real `UserDefaults`, and the store under
    /// test uses that spelling to clear the endpoint.
    func testSettingNilRemovesTheKeyAsARealUserDefaultsWould() {
        let defaults = TestUserDefaults.inMemory()
        defaults.set("value", forKey: "aKey")

        defaults.set(nil, forKey: "aKey")

        XCTAssertNil(defaults.object(forKey: "aKey"))
        XCTAssertFalse(defaults.everythingStored.keys.contains("aKey"))
    }

    // MARK: - What `everythingStored` means

    /// The reason it is not `dictionaryRepresentation()`. On a real
    /// `UserDefaults` that also returns the global and argument domains, so a
    /// test scanning it for a leaked secret could not tell "this store
    /// persisted the key" from "something else in the search list has a key of
    /// that name" — and `testTheApiKeyAppearsNowhereInUserDefaults` is exactly
    /// such a test.
    func testEverythingStoredContainsOnlyWhatWasWrittenThroughThisObject() {
        let defaults = TestUserDefaults.inMemory()

        defaults.set("one", forKey: "firstKey")
        defaults.set("two", forKey: "secondKey")

        XCTAssertEqual(
            Set(defaults.everythingStored.keys), ["firstKey", "secondKey"],
            """
            everythingStored must report this object's own writes and nothing \
            else — no global domain, no argument domain, no registered defaults
            """)
    }

    /// The consequence of overriding `object(forKey:)` outright rather than
    /// falling back to `super`: a key that exists in the process's real
    /// preferences is invisible here. That is the point — a test must not read
    /// whatever the machine it runs on happens to have stored.
    func testAKeyRegisteredOnThisObjectIsNotVisibleThroughIt() {
        let defaults = TestUserDefaults.inMemory()
        // A name no production code reads, deliberately. The registration
        // domain is volatile so this writes no file either way, but whether it
        // is scoped to the receiver or to the process is a Foundation detail
        // this test has no business depending on — and a real key name here
        // could change what a later test reading `.standard` sees.
        let key = "h1486RegistrationFallthroughProbe"

        defaults.register(defaults: [key: "https://registered.invalid"])

        XCTAssertNil(
            defaults.string(forKey: key),
            """
            nothing may reach this object except through set(_:forKey:): a \
            fallback to Foundation's domain search would make a test's result \
            depend on the preferences of the machine running it
            """)
    }

    func testTwoInMemoryStoresShareNothing() {
        let first = TestUserDefaults.inMemory()
        let second = TestUserDefaults.inMemory()

        first.set("first", forKey: "sharedKeyName")

        XCTAssertNil(
            second.string(forKey: "sharedKeyName"),
            "each test's storage must be its own, or one test's value leaks into the next")
    }

    /// Nothing written through the double reaches a persistent domain. This is
    /// the property that makes "it leaves no file" true, asserted the only way
    /// a single test can assert it without racing cfprefsd: by reading
    /// `.standard` rather than by counting files.
    func testAValueWrittenThroughTheDoubleIsNotVisibleInStandardDefaults() {
        let defaults = TestUserDefaults.inMemory()
        let key = "h1486InMemoryOnlyProbe"

        defaults.set("written-in-memory", forKey: key)

        XCTAssertNil(
            UserDefaults.standard.string(forKey: key),
            "a write through the double must not land in any persistent domain")
    }

    // MARK: - The one real suite

    /// The domain-separation test in `BundleIdentityTests` only means anything
    /// if the suite it opens is genuinely beneath the bundle identity, so that
    /// is pinned here rather than left to the literal staying right.
    func testTheOneRealSuiteIsNamedBeneathTheBundleIdentity() {
        XCTAssertTrue(
            TestUserDefaults.realSuiteName.hasPrefix("\(BundleIdentity.current)."),
            """
            the real suite must be a name below the running bundle's \
            identifier — that is the arrangement HORO-1456 rejected, and \
            showing it is a separate domain is the whole subject of the test \
            that opens it
            """)
        XCTAssertNotEqual(
            TestUserDefaults.realSuiteName, BundleIdentity.current,
            "a suite named exactly the running identifier is not a suffixed suite")
    }
}
