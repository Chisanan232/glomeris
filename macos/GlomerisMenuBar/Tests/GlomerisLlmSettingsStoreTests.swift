//
//  GlomerisLlmSettingsStoreTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1309 AC 9. Four properties are worth more than the rest of this
//  file put together, and they are all about the API key:
//
//    1. It never lands in UserDefaults. Asserted by dumping the whole
//       persistent domain and looking for the value, rather than by
//       checking the one key name this code happens to use — a future
//       `defaults.set(key, forKey: "llmApiKeyBackup")` has to fail too.
//    2. It reaches a child process only through the environment, never as
//       an argument.
//    3. Precedence is per field and settings-wins, so the settings screen
//       cannot show one endpoint while a request uses another.
//    4. The child environment is COMPLETE. `Process.environment` replaces
//       wholesale, so an overlay of three variables would strip PATH and
//       HOME and break executable resolution — HORO-1295's whole subject.
//
//  The keychain itself is deliberately not exercised here. An unsigned
//  xcodebuild test binary can get `errSecMissingEntitlement` from
//  SecItemAdd, and a test that skipped on that would be a permanently
//  green no-op — the HORO-1253 failure mode. Behaviour is asserted against
//  InMemoryCredentialStore; that production uses the keychain-backed store
//  is asserted mechanically by scripts/check-credential-store-uses-keychain.sh,
//  and KeychainCredentialStoreTests covers the real boundary by observing
//  whatever outcome it gets rather than demanding one.
//

import XCTest

final class GlomerisLlmSettingsStoreTests: XCTestCase {
    private static let key = "sk-test-only-never-a-real-credential"

    /// A fresh, uniquely-named suite per test, so no test sees another's
    /// state and none touches the real `dev.glomeris.GlomerisMenuBar` suite.
    private func makeDefaults() -> (UserDefaults, String) {
        let suiteName = "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        addTeardownBlock {
            defaults.removePersistentDomain(forName: suiteName)
        }
        return (defaults, suiteName)
    }

    private func makeStore(
        credentials: CredentialStore = InMemoryCredentialStore()
    ) -> (GlomerisLlmSettingsStore, UserDefaults, String) {
        let (defaults, suiteName) = makeDefaults()
        return (
            GlomerisLlmSettingsStore(defaults: defaults, credentials: credentials),
            defaults, suiteName
        )
    }

    // MARK: - The key never touches UserDefaults

    /// The property that matters most, and the one a plausible refactor
    /// breaks. Written to look for the *value* anywhere in the domain
    /// rather than at the one key name this implementation uses, because a
    /// well-meaning "cache it so the UI can show the last four characters"
    /// would add a different key and pass a narrower test.
    func testTheApiKeyAppearsNowhereInUserDefaults() {
        let (store, defaults, suiteName) = makeStore()
        store.endpoint = "https://api.example.test/v1"
        store.model = "a-model"
        store.setApiKey(Self.key)

        XCTAssertTrue(store.hasStoredApiKey, "the key must actually have been stored somewhere")

        let domain = defaults.persistentDomain(forName: suiteName) ?? [:]
        let dumped = domain.map { "\($0.key)=\(String(describing: $0.value))" }.joined(separator: "\n")
        XCTAssertFalse(
            dumped.contains(Self.key),
            "the API key is in UserDefaults, which is a plain plist on disk:\n\(dumped)"
        )
        // The endpoint and model *should* be there — otherwise this test
        // could pass against a store that persists nothing at all.
        XCTAssertTrue(dumped.contains("https://api.example.test/v1"))
        XCTAssertTrue(dumped.contains("a-model"))
    }

    func testTheStoreExposesNoWayToReadTheKeyBackForDisplay() {
        let (store, _, _) = makeStore()
        store.setApiKey(Self.key)

        // `hasStoredApiKey` is a Bool by design: the type has no property
        // returning the secret, so no view can bind to one. Asserted as a
        // reminder of why, since the compiler enforces it already.
        XCTAssertTrue(store.hasStoredApiKey)
        XCTAssertEqual(store.status(environment: [:]).apiKey, .settings)
    }

    // MARK: - Precedence (AC 3)

    func testStoredSettingsWinOverTheInheritedEnvironment() {
        let (store, _, _) = makeStore()
        store.endpoint = "https://configured.example.test/v1"
        store.model = "configured-model"
        store.setApiKey("configured-key")

        let environment = [
            "PATH": "/usr/bin",
            GlomerisLlmSettingsStore.endpointEnvironmentVariable: "https://inherited.example.test/v1",
            GlomerisLlmSettingsStore.modelEnvironmentVariable: "inherited-model",
            GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: "inherited-key",
        ]
        let child = store.childEnvironment(basedOn: environment)

        XCTAssertEqual(
            child[GlomerisLlmSettingsStore.endpointEnvironmentVariable],
            "https://configured.example.test/v1",
            "the settings screen would be showing a URL that is not the one being used"
        )
        XCTAssertEqual(child[GlomerisLlmSettingsStore.modelEnvironmentVariable], "configured-model")
        XCTAssertEqual(child[GlomerisLlmSettingsStore.apiKeyEnvironmentVariable], "configured-key")

        let status = store.status(environment: environment)
        XCTAssertEqual(status.endpoint, .settings)
        XCTAssertEqual(status.model, .settings)
        XCTAssertEqual(status.apiKey, .settings)
    }

    func testAnEmptyFieldFallsBackToTheInheritedVariableRatherThanClearingIt() {
        let (store, _, _) = makeStore()
        store.model = "configured-model"

        let environment = [
            GlomerisLlmSettingsStore.endpointEnvironmentVariable: "https://inherited.example.test/v1",
            GlomerisLlmSettingsStore.modelEnvironmentVariable: "inherited-model",
            GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: "inherited-key",
        ]
        let child = store.childEnvironment(basedOn: environment)

        XCTAssertEqual(
            child[GlomerisLlmSettingsStore.endpointEnvironmentVariable],
            "https://inherited.example.test/v1",
            "a field this app never filled in must not switch off a working shell setup"
        )
        XCTAssertEqual(child[GlomerisLlmSettingsStore.modelEnvironmentVariable], "configured-model")
        XCTAssertEqual(child[GlomerisLlmSettingsStore.apiKeyEnvironmentVariable], "inherited-key")

        let status = store.status(environment: environment)
        XCTAssertEqual(status.endpoint, .environment)
        XCTAssertEqual(status.model, .settings)
        XCTAssertEqual(status.apiKey, .environment)
        XCTAssertTrue(status.isComplete)
    }

    /// Precedence is decided per field, not per configuration. A user who
    /// types only a model must not lose an inherited endpoint, and the
    /// reverse must hold too — this is the mixed case the two tests above
    /// each cover only half of.
    func testPrecedenceIsDecidedFieldByField() {
        let (store, _, _) = makeStore()
        store.endpoint = "https://configured.example.test/v1"

        let environment = [
            GlomerisLlmSettingsStore.modelEnvironmentVariable: "inherited-model"
        ]
        let child = store.childEnvironment(basedOn: environment)

        XCTAssertEqual(
            child[GlomerisLlmSettingsStore.endpointEnvironmentVariable],
            "https://configured.example.test/v1")
        XCTAssertEqual(child[GlomerisLlmSettingsStore.modelEnvironmentVariable], "inherited-model")

        let status = store.status(environment: environment)
        XCTAssertEqual(status.endpoint, .settings)
        XCTAssertEqual(status.model, .environment)
        XCTAssertEqual(status.apiKey, .absent)
        XCTAssertFalse(
            status.isComplete,
            "no key from either place means a connection test would fail locally"
        )
    }

    /// An exported-but-empty `GLOMERIS_LLM_MODEL=` is not a configuration.
    /// The Rust `provider_from_parts` treats it as absent, and a settings
    /// screen claiming "inherited from your shell" for it would send the
    /// user looking for a value that is not there.
    func testAnEmptyInheritedVariableReadsAsAbsentJustAsTheCliSeesIt() {
        let (store, _, _) = makeStore()

        let status = store.status(environment: [
            GlomerisLlmSettingsStore.endpointEnvironmentVariable: "",
            GlomerisLlmSettingsStore.modelEnvironmentVariable: "   ",
            GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: "\n",
        ])

        XCTAssertEqual(status.endpoint, .absent)
        XCTAssertEqual(status.model, .absent)
        XCTAssertEqual(status.apiKey, .absent)
        XCTAssertFalse(status.isComplete)
    }

    // MARK: - The child environment stays complete

    /// `Process.environment` is a wholesale replacement, so anything this
    /// returns is the child's *entire* environment. Dropping PATH breaks
    /// `glomeris` lookup (HORO-1295) and dropping HOME breaks every
    /// HOME-bounded detector — silently, as a scan finding nothing.
    func testTheChildEnvironmentKeepsEverythingItWasGiven() {
        let (store, _, _) = makeStore()
        store.endpoint = "https://configured.example.test/v1"

        let environment = [
            "PATH": "/usr/local/bin:/usr/bin",
            "HOME": "/Users/someone",
            "TMPDIR": "/var/folders/xx/",
            "SOME_UNRELATED_VAR": "kept",
        ]
        let child = store.childEnvironment(basedOn: environment)

        for (name, value) in environment {
            XCTAssertEqual(child[name], value, "\(name) was dropped from the child environment")
        }
        XCTAssertEqual(child.count, environment.count + 1)
    }

    /// Nothing is ever removed. A GUI field the user clears is a fallback,
    /// not a kill switch — the opposite behaviour would let one stray
    /// keystroke in a settings window disable a setup the user configured
    /// in their shell, with no indication of what happened.
    func testNoInheritedVariableIsEverUnset() {
        let (store, _, _) = makeStore()
        store.endpoint = nil
        store.model = nil
        store.deleteApiKey()

        let environment = [
            GlomerisLlmSettingsStore.endpointEnvironmentVariable: "https://inherited.example.test/v1",
            GlomerisLlmSettingsStore.modelEnvironmentVariable: "inherited-model",
            GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: "inherited-key",
        ]

        XCTAssertEqual(store.childEnvironment(basedOn: environment), environment)
    }

    // MARK: - Storing, trimming, revoking (AC 8)

    func testAPastedKeyIsTrimmedBecauseATrailingNewlineIsAnUnexplainableFailure() {
        let credentials = InMemoryCredentialStore()
        let (store, _, _) = makeStore(credentials: credentials)

        store.setApiKey("  \(Self.key)\n")

        XCTAssertEqual(
            store.childEnvironment(basedOn: [:])[
                GlomerisLlmSettingsStore.apiKeyEnvironmentVariable],
            Self.key,
            "whitespace would reach the provider as part of the credential and read as a 401"
        )
    }

    func testDeletingTheKeyRemovesItAndReportsTheFieldAsAbsent() {
        let (store, _, _) = makeStore()
        store.setApiKey(Self.key)
        XCTAssertTrue(store.hasStoredApiKey)

        XCTAssertTrue(store.deleteApiKey())

        XCTAssertFalse(store.hasStoredApiKey)
        XCTAssertEqual(store.status(environment: [:]).apiKey, .absent)
        XCTAssertNil(
            store.childEnvironment(basedOn: [:])[
                GlomerisLlmSettingsStore.apiKeyEnvironmentVariable],
            "a revoked key must stop reaching the child process"
        )
    }

    /// "Clear the field and save" and "revoke" must not disagree about what
    /// happened. Storing an empty string deletes, so a user who selects the
    /// key and presses delete does not end up with an empty credential
    /// stored and a 401 they cannot explain.
    func testStoringAnEmptyKeyDeletesRatherThanStoringNothing() {
        let (store, _, _) = makeStore()
        store.setApiKey(Self.key)

        store.setApiKey("   ")

        XCTAssertFalse(store.hasStoredApiKey)
    }

    func testDeletingAKeyThatWasNeverStoredSucceeds() {
        let (store, _, _) = makeStore()

        XCTAssertTrue(
            store.deleteApiKey(),
            "the postcondition is 'no key is stored', and it already held"
        )
    }

    func testSettingsSurviveAFreshStoreOverTheSameDefaults() {
        let credentials = InMemoryCredentialStore()
        let (defaults, _) = makeDefaults()
        let store = GlomerisLlmSettingsStore(defaults: defaults, credentials: credentials)
        store.endpoint = " https://api.example.test/v1 "
        store.model = "a-model"
        store.setApiKey(Self.key)

        // A fresh instance over the same backing stores simulates a restart:
        // reading must not depend on any in-memory cache.
        let reopened = GlomerisLlmSettingsStore(defaults: defaults, credentials: credentials)
        XCTAssertEqual(reopened.endpoint, "https://api.example.test/v1", "a pasted URL is trimmed")
        XCTAssertEqual(reopened.model, "a-model")
        XCTAssertTrue(reopened.hasStoredApiKey)
    }

    /// The settings store validates no URL. `validate_base_url` in the Rust
    /// CLI is the only judge of what can work, and `llm-check` is how its
    /// verdict reaches the user — a second, drifting copy of those rules in
    /// Swift is exactly what the standing project rule forbids.
    func testAnUnusableEndpointIsStoredAsTypedRatherThanSilentlyCorrected() {
        let (store, _, _) = makeStore()

        store.endpoint = "api.example.test/v1/chat/completions"

        XCTAssertEqual(store.endpoint, "api.example.test/v1/chat/completions")
    }

    // MARK: - Asking without reading (HORO-1368)

    /// The change the whole ticket rests on. Establishing whether a key is
    /// stored must not retrieve it: retrieving decrypts, decrypting consults the
    /// item's ACL, and a build the ACL does not admit gets an authorisation
    /// prompt instead of an answer — on the main thread, for as long as nobody
    /// answers it.
    ///
    /// Before this, `status()` called `secret(forKey:)`. That is exactly the
    /// touch this asserts is gone, so this test fails against the old
    /// implementation rather than merely describing the new one.
    func testAskingWhereTheKeyCameFromNeverRetrievesIt() {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": Self.key])
        let (store, _, _) = makeStore(credentials: credentials)

        let status = store.status(environment: [:])

        XCTAssertEqual(status.apiKey, .settings, "it must still get the right answer")
        XCTAssertEqual(
            credentials.operations, [.availability],
            "one existence query and nothing else; \(credentials.operations)")
    }

    /// The one call that legitimately needs the value, asserted as the only one,
    /// so "the key is read at spawn time and nowhere else" is a measured claim.
    func testOnlyBuildingAChildEnvironmentRetrievesTheKey() {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": Self.key])
        let (store, _, _) = makeStore(credentials: credentials)

        _ = store.status(environment: [:])
        _ = store.hasStoredApiKey
        _ = store.endpoint
        _ = store.model
        XCTAssertFalse(credentials.operations.contains(.secret))

        _ = store.childEnvironment(basedOn: [:])

        XCTAssertEqual(
            credentials.operations.filter { $0 == .secret }.count, 1,
            "the value is read once, at the moment a child process is spawned")
    }

    // MARK: - A refused read is not an absent key (AC 6)

    /// The state that had no wording at all before this ticket, because it took
    /// the menu-bar item with it and left nothing on screen to word.
    ///
    /// `unreadableKeys` is the only way to reproduce it without a keychain: the
    /// real condition needs a binary whose code identity an existing item's ACL
    /// rejects, which cannot be arranged from inside the test bundle that would
    /// have to observe it.
    func testAKeyTheKeychainRefusesIsReportedAsUnreadableRatherThanAbsent() {
        let credentials = InMemoryCredentialStore(
            secrets: ["llmApiKey": Self.key], unreadableKeys: ["llmApiKey"])
        let (store, _, _) = makeStore(credentials: credentials)

        let status = store.status(environment: [:])

        XCTAssertEqual(store.apiKeyAvailability, .unreadable)
        XCTAssertEqual(
            status.apiKey, .unreadable,
            "'Not set' about a key that is set sends the user to paste one they already have")
        XCTAssertFalse(
            status.isComplete,
            "a key that cannot be read cannot reach the CLI, so a test would fail locally")
        XCTAssertFalse(
            store.hasStoredApiKey,
            "the Bool cannot express this, which is why the UI does not use it")
        XCTAssertNil(
            store.childEnvironment(basedOn: [:])[
                GlomerisLlmSettingsStore.apiKeyEnvironmentVariable],
            "nothing readable means nothing to pass on — the CLI must refuse, not get a blank")
    }

    /// An inherited key wins over an unreadable one, and does so silently. In
    /// that case the product genuinely works — the child process gets the
    /// inherited key — so there is nothing for the user to fix and a warning
    /// would be noise about a working setup.
    func testAnInheritedKeyOutranksAnUnreadableStoredOneWithoutComplaining() {
        let credentials = InMemoryCredentialStore(
            secrets: ["llmApiKey": Self.key], unreadableKeys: ["llmApiKey"])
        let (store, _, _) = makeStore(credentials: credentials)
        store.endpoint = "https://configured.example.test/v1"
        store.model = "a-model"

        let environment = [
            GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: "inherited-key"
        ]
        let status = store.status(environment: environment)

        XCTAssertEqual(status.apiKey, .environment)
        XCTAssertTrue(status.isComplete, "this configuration works; it must not read as broken")
        XCTAssertEqual(
            store.childEnvironment(basedOn: environment)[
                GlomerisLlmSettingsStore.apiKeyEnvironmentVariable],
            "inherited-key")
    }

    /// The full truth table for the one field whose storage can refuse the
    /// question, since three of the four rows are new and the interesting part is
    /// which of availability and inheritance wins in each.
    func testTheKeySourceTruthTable() {
        let cases: [(CredentialAvailability, String?, GlomerisLlmSettingSource)] = [
            (.present, nil, .settings),
            (.present, "inherited", .settings),
            (.absent, nil, .absent),
            (.absent, "inherited", .environment),
            (.unreadable, nil, .unreadable),
            (.unreadable, "inherited", .environment),
            // An exported-but-empty variable is not an inheritance, exactly as
            // the Rust `provider_from_parts` reads it.
            (.unreadable, "   ", .unreadable),
            (.absent, "", .absent),
        ]

        for (availability, inherited, expected) in cases {
            let unreadable: Set<String> = availability == .unreadable ? ["llmApiKey"] : []
            let secrets = availability == .absent ? [:] : ["llmApiKey": Self.key]
            let (store, _, _) = makeStore(
                credentials: InMemoryCredentialStore(
                    secrets: secrets, unreadableKeys: unreadable))
            let environment =
                inherited.map { [GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: $0] } ?? [:]

            XCTAssertEqual(
                store.status(environment: environment).apiKey, expected,
                "availability \(availability) with inherited \(inherited ?? "nil")")
        }
    }

    // MARK: - Off the main thread (AC 1, AC 3)

    /// The measured claim behind the fix. Every keychain touch must happen
    /// somewhere other than the main thread, because a keychain operation can
    /// wait on a person and a main thread that is waiting on a person cannot
    /// service the status item — AppKit removes it, and the app disappears from
    /// the menu bar with no way back in.
    ///
    /// `@MainActor` so the premise is real: these calls start on the thread the
    /// app's UI runs on, which is the thread the bug was about.
    ///
    /// What this catches, measured: replacing the store's hop with
    /// `await MainActor.run { work() }` fails this test and the one in
    /// `AiProviderPreferencesViewTests`. Replacing it with a bare `work()` does
    /// NOT — awaiting a nonisolated `async` function from a main-actor caller
    /// already leaves the main actor, so that variant is off-thread too. The
    /// queue's own contribution is serialisation, and
    /// `testConcurrentResolutionIsSerialisedSoOnlyOnePromptCanEverBeOutstanding`
    /// is the test that covers it.
    @MainActor
    func testEveryResolvedKeychainTouchHappensOffTheMainThread() async {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": Self.key])
        let (store, _, _) = makeStore(credentials: credentials)
        XCTAssertTrue(Thread.isMainThread, "the premise of this test")

        _ = await store.resolvedStatus(environment: [:])
        _ = await store.resolvedChildEnvironment(basedOn: [:])
        _ = await store.resolvedSetApiKey("another-test-only-value")
        _ = await store.resolvedDeleteApiKey()

        XCTAssertEqual(
            Set(credentials.touches.map(\.operation)),
            [.availability, .secret, .set, .delete],
            "all four kinds of touch are covered: \(credentials.touches)")
        for touch in credentials.touches {
            XCTAssertFalse(
                touch.wasOnMainThread,
                "\(touch.operation) ran on the main thread, which is the HORO-1368 defect")
        }
    }

    /// Anti-vacuity for the test above: the recorder really can tell the main
    /// thread apart, so "nothing ran on it" is a finding rather than a recorder
    /// that always reports `false`. The synchronous members are what the
    /// `resolved*` ones call, and calling one directly from here must be
    /// recorded as main-thread.
    @MainActor
    func testTheThreadRecorderActuallyDetectsTheMainThread() {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": Self.key])
        let (store, _, _) = makeStore(credentials: credentials)

        _ = store.status(environment: [:])

        XCTAssertEqual(credentials.touches.count, 1)
        XCTAssertTrue(
            credentials.touches[0].wasOnMainThread,
            "if this is false the off-thread assertions above prove nothing")
    }

    /// Two overlapping reads of one ACL-guarded item can raise two authorisation
    /// prompts, and a user facing a stack of identical prompts cannot tell
    /// whether answering one did anything. The queue is serial for that reason,
    /// so no two touches may overlap however many callers there are.
    func testConcurrentResolutionIsSerialisedSoOnlyOnePromptCanEverBeOutstanding() async {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": Self.key])
        let (store, _, _) = makeStore(credentials: credentials)

        await withTaskGroup(of: Void.self) { group in
            for _ in 0..<8 {
                group.addTask { _ = await store.resolvedStatus(environment: [:]) }
                group.addTask { _ = await store.resolvedChildEnvironment(basedOn: [:]) }
            }
        }

        XCTAssertEqual(credentials.touches.count, 16, "every call must have happened")
        XCTAssertEqual(
            credentials.maxConcurrentTouches, 1,
            "two keychain operations were in flight at once, which is two prompts")
    }
}

/// A `CredentialStore` that records what was asked of it, and from where.
///
/// Exists for two claims a value-returning double cannot make: that establishing
/// whether a key exists does not retrieve it (the substitution HORO-1368 turns
/// on), and that every touch happens off the main thread. Both are about the
/// calls made rather than the answers given, so the double has to be a recorder.
final class RecordingCredentialStore: CredentialStore, @unchecked Sendable {
    enum Operation: Equatable {
        case availability
        case secret
        case set
        case delete
    }

    struct Touch: Equatable, CustomStringConvertible {
        let operation: Operation
        let wasOnMainThread: Bool

        var description: String {
            "\(operation)\(wasOnMainThread ? " on the main thread" : "")"
        }
    }

    private let lock = NSLock()
    private var recorded: [Touch] = []
    private var secrets: [String: String]
    private var inFlight = 0
    private var peakInFlight = 0

    init(secrets: [String: String] = [:]) {
        self.secrets = secrets
    }

    var touches: [Touch] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    var operations: [Operation] { touches.map(\.operation) }

    /// The high-water mark of overlapping calls. `1` means the caller serialised
    /// them; anything more means two keychain operations — so potentially two
    /// authorisation prompts — were outstanding together.
    var maxConcurrentTouches: Int {
        lock.lock()
        defer { lock.unlock() }
        return peakInFlight
    }

    /// Records the call, then holds briefly so genuinely concurrent callers
    /// overlap observably. Without the hold, a caller that did fan out could
    /// still finish each call before starting the next and look serial.
    private func record<T>(_ operation: Operation, _ work: () -> T) -> T {
        let touch = Touch(operation: operation, wasOnMainThread: Thread.isMainThread)
        lock.lock()
        recorded.append(touch)
        inFlight += 1
        peakInFlight = max(peakInFlight, inFlight)
        lock.unlock()

        let result = work()

        Thread.sleep(forTimeInterval: 0.005)
        lock.lock()
        inFlight -= 1
        lock.unlock()
        return result
    }

    func availability(forKey key: String) -> CredentialAvailability {
        record(.availability) {
            lock.lock()
            defer { lock.unlock() }
            return secrets[key]?.isEmpty == false ? .present : .absent
        }
    }

    func secret(forKey key: String) -> String? {
        record(.secret) {
            lock.lock()
            defer { lock.unlock() }
            guard let secret = secrets[key], !secret.isEmpty else { return nil }
            return secret
        }
    }

    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool {
        record(.set) {
            lock.lock()
            defer { lock.unlock() }
            secrets[key] = secret
            return true
        }
    }

    @discardableResult
    func deleteSecret(forKey key: String) -> Bool {
        record(.delete) {
            lock.lock()
            defer { lock.unlock() }
            secrets.removeValue(forKey: key)
            return true
        }
    }
}

/// The real keychain, covered by observing what it does rather than
/// requiring a particular outcome.
///
/// On an unsigned test binary `SecItemAdd` may return
/// `errSecMissingEntitlement`. The assertions below are therefore all
/// conditional on the store having accepted the write — but the
/// conditional is `XCTSkip`-free and asserts the *negative* unconditionally:
/// whatever happens, a failed write must not leave a readable secret, and a
/// delete must leave nothing behind. A CI run where the keychain is
/// unavailable still checks something real.
final class KeychainCredentialStoreTests: XCTestCase {
    /// Namespaced away from the app's own service so a test can never read,
    /// overwrite or delete the developer's real key.
    private func makeStore() -> (KeychainCredentialStore, String) {
        let service = "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)"
        let store = KeychainCredentialStore(service: service)
        let account = "llmApiKey"
        addTeardownBlock { store.deleteSecret(forKey: account) }
        return (store, account)
    }

    func testAStoredSecretReadsBackAndADeletedOneDoesNot() throws {
        let (store, account) = makeStore()
        let secret = "sk-test-only-\(UUID().uuidString)"

        let stored = store.setSecret(secret, forKey: account)
        if stored {
            XCTAssertEqual(store.secret(forKey: account), secret)
        } else {
            // The write was refused — on an unsigned binary this is
            // `errSecMissingEntitlement`. Nothing may be readable either way.
            XCTAssertNil(
                store.secret(forKey: account),
                "a refused write must not leave a partially stored secret"
            )
        }

        XCTAssertTrue(store.deleteSecret(forKey: account))
        XCTAssertNil(store.secret(forKey: account), "a deleted secret must be gone")
    }

    func testOverwritingReplacesRatherThanAccumulating() throws {
        let (store, account) = makeStore()

        guard store.setSecret("first", forKey: account) else {
            // Recorded rather than skipped: a skip on a machine where the
            // keychain is unavailable is how a gate goes permanently green.
            XCTAssertNil(store.secret(forKey: account))
            return
        }
        XCTAssertTrue(store.setSecret("second", forKey: account))

        XCTAssertEqual(
            store.secret(forKey: account),
            "second",
            "an update must replace in place, not leave the old key readable"
        )
    }

    func testAnAbsentSecretIsNilRatherThanAnEmptyString() {
        let (store, account) = makeStore()

        XCTAssertNil(store.secret(forKey: account))
    }

    /// Two services are two items. Asserted because the account name is
    /// shared (`llmApiKey`) and a query missing `kSecAttrService` would
    /// return whichever item the keychain felt like.
    func testItemsAreScopedToTheirService() {
        let (storeA, account) = makeStore()
        let (storeB, _) = makeStore()

        guard storeA.setSecret("belongs-to-a", forKey: account) else {
            XCTAssertNil(storeA.secret(forKey: account))
            return
        }

        XCTAssertNil(
            storeB.secret(forKey: account),
            "one app's stored key is readable through another service name"
        )
    }
}

final class InMemoryCredentialStoreTests: XCTestCase {
    /// The double must behave like the real thing on the two points the
    /// tests above depend on, or those tests prove nothing about production:
    /// an absent key is `nil`, and so is a stored empty string.
    func testTheDoubleMatchesTheRealStoreOnAbsenceAndEmptiness() {
        let store = InMemoryCredentialStore()

        XCTAssertNil(store.secret(forKey: "llmApiKey"))

        store.setSecret("", forKey: "llmApiKey")
        XCTAssertNil(store.secret(forKey: "llmApiKey"), "an empty secret is not a secret")

        store.setSecret("something", forKey: "llmApiKey")
        XCTAssertEqual(store.secret(forKey: "llmApiKey"), "something")

        XCTAssertTrue(store.deleteSecret(forKey: "llmApiKey"))
        XCTAssertNil(store.secret(forKey: "llmApiKey"))
    }
}
