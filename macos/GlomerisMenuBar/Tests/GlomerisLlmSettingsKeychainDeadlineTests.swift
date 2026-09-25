//
//  GlomerisLlmSettingsKeychainDeadlineTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1471 AC 4 and AC 5. What happens when the keychain does not answer.
//
//  These live in their own file rather than beside the other settings-store
//  tests because they need something the others must never have: a credential
//  store whose availability query does not return. `InMemoryCredentialStore`
//  cannot stand in — it answers immediately, and the subject here is the
//  absence of an answer.
//
//  Two rules this file obeys, both of which cost something to get wrong:
//
//   1. Every wait is bounded by an XCTestExpectation, never by awaiting the
//      store directly from an `async` test. If the deadline is removed from
//      the store, `resolvedStatus` never returns; an `async` test would hang
//      until XCTest's own per-test limit and report a timeout in place of a
//      failure. `resolveStatus(of:)` below turns that into an ordinary, named
//      assertion failure — which is what AC 5 asks for.
//
//   2. No test here touches `KeychainDeadline.shared`. The record of a missed
//      deadline is deliberately process-wide (a per-instance record would make
//      the app wait out the deadline again on every re-evaluation of the
//      settings screen), so a single test that let the shared record be missed
//      would make every *other* keychain test in this bundle resolve as
//      `.unresponsive` without asking anything — a bundle-wide false green,
//      arriving or not depending on test order. Each test injects its own.
//      That the no-argument initialiser wires up `.shared` is therefore
//      asserted by reading, not by stalling it; see
//      `testTheRecordIsStickyAndTheShippedDeadlineIsThreeSeconds`.
//
//  The stall also has to be cleaned up. A work item blocked on the store's
//  shared serial keychain queue holds that queue until it returns, so leaving
//  one blocked would strand the keychain tests in the sibling file. Every
//  stalled store registers a teardown block that releases it and then waits
//  for the queue to drain, and fails the test if it does not.
//

import XCTest

final class GlomerisLlmSettingsKeychainDeadlineTests: XCTestCase {
    /// Short enough to keep the suite quick, long enough that "it did not
    /// return instantly" is a measurement rather than a coin toss.
    private static let deadlineSeconds: TimeInterval = 0.3

    /// Generous next to `deadlineSeconds`: this bound exists to convert a hang
    /// into a failure, not to assert how fast the deadline is. The tightness
    /// lives in `XCTAssertLessThan` on the measured elapsed time instead.
    private static let waitSeconds: TimeInterval = 5

    private static let key = "sk-test-only-never-a-real-credential"

    // MARK: - A keychain that goes quiet

    /// The double AC 4 asks for: an availability query that does not return.
    ///
    /// It blocks on a semaphore rather than sleeping, so the test decides when
    /// it ends, and it counts entries and exits so a test can assert the query
    /// was issued exactly once and a teardown can tell when the queue is clear.
    ///
    /// The bounded `wait` is a safety net, not the mechanism: if a future edit
    /// loses the teardown release, the process recovers after a few seconds
    /// instead of stranding the rest of the bundle behind it.
    private final class StalledCredentialStore: CredentialStore, @unchecked Sendable {
        private let gate = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var entered = 0
        private var exited = 0

        /// What the query answers once it is finally allowed to. `.present` on
        /// purpose: a test that released the gate and still saw "no key here"
        /// would be reading the double's own emptiness rather than the stall.
        private let eventualAnswer: CredentialAvailability = .present

        var availabilityCalls: Int {
            lock.lock()
            defer { lock.unlock() }
            return entered
        }

        var isStillBlocked: Bool {
            lock.lock()
            defer { lock.unlock() }
            return exited < entered
        }

        /// Lets every blocked query through, and every later one straight
        /// through. Over-signalling a semaphore is harmless; under-signalling
        /// it would leave the queue held.
        func release() {
            for _ in 0..<64 { gate.signal() }
        }

        func availability(forKey key: String) -> CredentialAvailability {
            lock.lock()
            entered += 1
            lock.unlock()
            _ = gate.wait(timeout: .now() + 10)
            lock.lock()
            exited += 1
            lock.unlock()
            return eventualAnswer
        }

        func secret(forKey key: String) -> String? { nil }

        @discardableResult
        func setSecret(_ secret: String, forKey key: String) -> Bool { true }

        @discardableResult
        func deleteSecret(forKey key: String) -> Bool { true }
    }

    /// Carries a result out of a `Task` without capturing a `var` across
    /// concurrency domains.
    private final class StatusBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: GlomerisLlmSettingsStatus?
        private var seconds: TimeInterval = 0

        func store(_ status: GlomerisLlmSettingsStatus, after seconds: TimeInterval) {
            lock.lock()
            defer { lock.unlock() }
            stored = status
            self.seconds = seconds
        }

        var value: (status: GlomerisLlmSettingsStatus, elapsed: TimeInterval)? {
            lock.lock()
            defer { lock.unlock() }
            guard let stored else { return nil }
            return (stored, seconds)
        }
    }

    // MARK: - Fixtures

    private func makeDefaults() -> UserDefaults {
        let suiteName = "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        addTeardownBlock {
            defaults.removePersistentDomain(forName: suiteName)
        }
        return defaults
    }

    /// A store whose keychain will not answer, wired to its own deadline
    /// record, with the release-and-drain teardown already registered.
    private func makeStalledStore(
        deadline: KeychainDeadline? = nil
    ) -> (GlomerisLlmSettingsStore, StalledCredentialStore, KeychainDeadline) {
        let credentials = StalledCredentialStore()
        let deadline = deadline ?? KeychainDeadline(seconds: Self.deadlineSeconds)
        let store = GlomerisLlmSettingsStore(
            defaults: makeDefaults(),
            credentials: credentials,
            keychainDeadline: deadline)
        addTeardownBlock { [weak self] in
            credentials.release()
            self?.waitForTheKeychainQueueToDrain(credentials)
        }
        return (store, credentials, deadline)
    }

    /// The abandoned work item is still holding the store's serial keychain
    /// queue. Nothing can cancel it, so the only way to hand a usable queue to
    /// the next test is to let it finish and wait for it.
    private func waitForTheKeychainQueueToDrain(
        _ credentials: StalledCredentialStore,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let limit = Date().addingTimeInterval(5)
        while credentials.isStillBlocked && Date() < limit {
            Thread.sleep(forTimeInterval: 0.01)
        }
        XCTAssertFalse(
            credentials.isStillBlocked,
            """
            A keychain query is still blocked, so the store's serial queue is \
            still held and the keychain tests in the sibling file would resolve \
            against a poisoned queue.
            """,
            file: file, line: line)
    }

    /// Resolves the status under a wall-clock bound, and returns `nil` having
    /// failed the test if it did not finish.
    ///
    /// This is the shape AC 5 needs. Remove the deadline from the store and
    /// `resolvedStatus` never returns: the expectation is never fulfilled, this
    /// fails by name, and the caller's remaining assertions are skipped rather
    /// than crashing on a missing value.
    private func resolveStatus(
        of store: GlomerisLlmSettingsStore,
        environment: [String: String] = [:],
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> (status: GlomerisLlmSettingsStatus, elapsed: TimeInterval)? {
        let box = StatusBox()
        let returned = expectation(description: "resolvedStatus returned")
        Task {
            let start = DispatchTime.now().uptimeNanoseconds
            let status = await store.resolvedStatus(environment: environment)
            let seconds =
                Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000_000
            box.store(status, after: seconds)
            returned.fulfill()
        }
        let outcome = XCTWaiter.wait(for: [returned], timeout: Self.waitSeconds)
        guard outcome == .completed, let value = box.value else {
            XCTFail(
                """
                resolvedStatus did not return within \(Self.waitSeconds)s. The \
                keychain deadline is what makes it return at all when the \
                keychain does not answer.
                """,
                file: file, line: line)
            return nil
        }
        return value
    }

    // MARK: - AC 1: silence is not absence

    /// The failure the ticket is about. Before this change the settings screen
    /// waited on the keychain with no bound, so the card stayed on its pending
    /// state forever; the tempting fix — treat no answer as no key — would
    /// print "Not set" about a key the user may well have saved, which is why
    /// `.absent` is asserted against explicitly and not merely left untested.
    func testAKeychainThatNeverAnswersIsReportedAsUnresponsiveAndNotAsAbsent() {
        let (store, credentials, _) = makeStalledStore()

        guard let (status, elapsed) = resolveStatus(of: store) else { return }

        XCTAssertEqual(status.apiKey, .unresponsive)
        XCTAssertNotEqual(
            status.apiKey, .absent,
            "\"Not set\" is a false statement about the user's data when nothing answered")
        XCTAssertNotNil(status.apiKey, "the card must not stay pending")
        XCTAssertEqual(
            status.apiKey?.isAvailable, false,
            "an unknown key must not be counted as one we have")
        XCTAssertFalse(status.isComplete)

        XCTAssertGreaterThanOrEqual(
            elapsed, Self.deadlineSeconds * 0.8,
            """
            It answered without waiting, which would mean the query never ran \
            and this test proves nothing.
            """)
        XCTAssertEqual(credentials.availabilityCalls, 1)
    }

    // MARK: - AC 2: and the next query does not wait again

    /// The second half of the defect: the query that stalled is holding the
    /// store's serial keychain queue, so a later query cannot get an answer
    /// either — it would sit out its own deadline, once per re-evaluation of
    /// the settings screen, for the life of the process. Recording the miss is
    /// what makes the second answer immediate.
    func testTheStalledQueryIsNotIssuedASecondTime() {
        let (store, credentials, deadline) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)
        XCTAssertTrue(deadline.hasBeenMissed)

        guard let second = resolveStatus(of: store) else { return }
        XCTAssertEqual(second.status.apiKey, .unresponsive)
        XCTAssertLessThan(
            second.elapsed, Self.deadlineSeconds / 3,
            "the second query waited again instead of reading the record")
        XCTAssertEqual(
            credentials.availabilityCalls, 1,
            "the keychain was asked a second time")
    }

    /// The settings screen builds a store each time SwiftUI re-evaluates it, so
    /// a record held per store instance would be no record at all. Same
    /// credential store, same deadline, new store.
    func testAFreshStoreInTheSameSessionStillKnows() {
        let (store, credentials, deadline) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)

        let rebuilt = GlomerisLlmSettingsStore(
            defaults: makeDefaults(),
            credentials: credentials,
            keychainDeadline: deadline)
        guard let again = resolveStatus(of: rebuilt) else { return }

        XCTAssertEqual(again.status.apiKey, .unresponsive)
        XCTAssertLessThan(again.elapsed, Self.deadlineSeconds / 3)
        XCTAssertEqual(credentials.availabilityCalls, 1)
    }

    // MARK: - What silence must not cost

    /// An inherited key is read from the environment and needs no keychain at
    /// all, so a quiet keychain must not hide one. The opposite behaviour would
    /// tell a user who exported the variable that their key is unknown while
    /// the CLI was using it perfectly well.
    func testAnInheritedKeyStillOutranksTheSilence() {
        let (store, _, _) = makeStalledStore()

        guard let (status, _) = resolveStatus(
            of: store,
            environment: [GlomerisLlmSettingsStore.apiKeyEnvironmentVariable: Self.key]
        ) else { return }

        XCTAssertEqual(status.apiKey, .environment)
        XCTAssertEqual(status.apiKey?.isAvailable, true)
    }

    /// The endpoint and the model come from UserDefaults and the environment.
    /// They were never the keychain's business, and a stalled keychain must not
    /// take the whole card down with it.
    func testTheEndpointAndModelStillResolveWhenTheKeychainDoesNot() {
        let (store, _, _) = makeStalledStore()

        guard let (status, _) = resolveStatus(
            of: store,
            environment: [
                GlomerisLlmSettingsStore.endpointEnvironmentVariable:
                    "https://api.example.test/v1",
                GlomerisLlmSettingsStore.modelEnvironmentVariable: "a-model",
            ]
        ) else { return }

        XCTAssertEqual(status.endpoint, .environment)
        XCTAssertEqual(status.model, .environment)
        XCTAssertEqual(status.apiKey, .unresponsive, "the key is the only casualty")
    }

    // MARK: - Anti-vacuity

    /// The control for every test above. The same double, allowed to answer,
    /// resolves to `.settings` straight away — so `.unresponsive` above came
    /// from the silence and not from a double that cannot report a key at all,
    /// and the elapsed-time assertions are measuring a real wait.
    func testTheSameDoubleAnswersImmediatelyOnceItIsAllowedTo() {
        let (store, credentials, deadline) = makeStalledStore()
        credentials.release()

        guard let (status, elapsed) = resolveStatus(of: store) else { return }

        XCTAssertEqual(status.apiKey, .settings)
        XCTAssertLessThan(elapsed, Self.deadlineSeconds / 3)
        XCTAssertFalse(
            deadline.hasBeenMissed,
            "a deadline that is recorded as missed when the keychain answered would latch the whole session")
        XCTAssertEqual(credentials.availabilityCalls, 1)
    }

    // MARK: - The record itself

    /// Two properties of the record, asserted directly.
    ///
    /// The shipped wiring — that the no-argument initialiser takes
    /// `KeychainDeadline.shared` — is asserted by reading the initialiser and
    /// not by exercising it, deliberately. Missing the shared record from
    /// inside this process would make every later keychain test in this bundle
    /// resolve as `.unresponsive` without asking anything, and which tests
    /// those were would depend on run order. It was instead exercised out of
    /// process against these same sources while the fix was written.
    func testTheRecordIsStickyAndTheShippedDeadlineIsThreeSeconds() {
        let deadline = KeychainDeadline(seconds: Self.deadlineSeconds)
        XCTAssertFalse(deadline.hasBeenMissed)
        deadline.recordMiss()
        XCTAssertTrue(deadline.hasBeenMissed)
        deadline.recordMiss()
        XCTAssertTrue(deadline.hasBeenMissed, "a miss must not be cleared by a second miss")

        XCTAssertEqual(
            KeychainDeadline.defaultSeconds, 3,
            """
            The wait a user sits through once per session. Long enough that a \
            merely slow keychain is not called unresponsive, short enough that \
            the settings screen is not read as hung.
            """)
        XCTAssertEqual(
            deadline.seconds, Self.deadlineSeconds,
            "an injected deadline must be the one that is used")
    }
}
