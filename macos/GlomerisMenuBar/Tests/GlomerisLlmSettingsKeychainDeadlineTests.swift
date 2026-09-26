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
        private var accessEntered = 0
        private var accessExited = 0
        private var writes = 0
        private var deletes = 0

        /// What the query answers once it is finally allowed to. `.present` on
        /// purpose: a test that released the gate and still saw "no key here"
        /// would be reading the double's own emptiness rather than the stall.
        private let eventualAnswer: CredentialAvailability = .present

        var availabilityCalls: Int {
            lock.lock()
            defer { lock.unlock() }
            return entered
        }

        /// HORO-1474: the ACL query, counted separately from the availability
        /// one. Sharing a counter would make "the second query was never issued"
        /// unprovable — the interesting claim is that a miss recorded by *either*
        /// query stops the *other*.
        var accessListCalls: Int {
            lock.lock()
            defer { lock.unlock() }
            return accessEntered
        }

        var isStillBlocked: Bool {
            lock.lock()
            defer { lock.unlock() }
            return exited < entered || accessExited < accessEntered
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

        /// Stalls exactly as the availability query does, because it is the same
        /// hazard: a synchronous call into `securityd` with no bound of its own.
        ///
        /// Its eventual answer is a non-empty list for the same reason
        /// `eventualAnswer` is `.present` — a test that released the gate and saw
        /// an empty list could be reading the double's emptiness rather than the
        /// stall.
        func accessList(forKey key: String) -> CredentialAccessListReading {
            lock.lock()
            accessEntered += 1
            lock.unlock()
            _ = gate.wait(timeout: .now() + 10)
            lock.lock()
            accessExited += 1
            lock.unlock()
            return .applications([
                CredentialTrustedApplication(
                    reference: .resolves(path: "/Applications/Glomeris.app"), index: 0)
            ])
        }

        func secret(forKey key: String) -> String? { nil }

        /// HORO-1476: the two writes, counted, and answering `true` when they do
        /// run.
        ///
        /// They do not block. The subject of the write tests is not a write that
        /// stalls — it is a write that is never issued because the *availability*
        /// query stalled first and is still holding the one serial queue every
        /// keychain touch shares. A blocking write here would prove the queue is
        /// serial, which a sibling test already establishes, and would hide the
        /// thing being asserted.
        ///
        /// `true` rather than `false` so that a test seeing `.notAttempted` cannot
        /// be reading a double that refuses everything. The distinction between
        /// "refused" and "not attempted" is the whole point, and a double that can
        /// only refuse could not show it.
        var writeCalls: Int {
            lock.lock()
            defer { lock.unlock() }
            return writes
        }

        var deleteCalls: Int {
            lock.lock()
            defer { lock.unlock() }
            return deletes
        }

        @discardableResult
        func setSecret(_ secret: String, forKey key: String) -> Bool {
            lock.lock()
            writes += 1
            lock.unlock()
            return true
        }

        @discardableResult
        func deleteSecret(forKey key: String) -> Bool {
            lock.lock()
            deletes += 1
            lock.unlock()
            return true
        }
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

    /// The same shape for the ACL query (HORO-1474 AC 5).
    ///
    /// A separate helper rather than a generic one, so that removing the deadline
    /// from `resolvedAccessList()` alone fails here by name instead of somewhere
    /// shared. Without it that edit hangs: `XCTWaiter` converts the hang into a
    /// named failure and the caller's remaining assertions are skipped.
    private func resolveAccessList(
        of store: GlomerisLlmSettingsStore,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> (access: GlomerisCredentialAccess, elapsed: TimeInterval)? {
        let box = AccessBox()
        let returned = expectation(description: "resolvedAccessList returned")
        Task {
            let start = DispatchTime.now().uptimeNanoseconds
            let access = await store.resolvedAccessList()
            let seconds =
                Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000_000
            box.store(access, after: seconds)
            returned.fulfill()
        }
        let outcome = XCTWaiter.wait(for: [returned], timeout: Self.waitSeconds)
        guard outcome == .completed, let value = box.value else {
            XCTFail(
                """
                resolvedAccessList did not return within \(Self.waitSeconds)s. The \
                ACL read is a synchronous call into securityd with no bound of its \
                own, so the shared keychain deadline is what makes it return at \
                all when the keychain does not answer.
                """,
                file: file, line: line)
            return nil
        }
        return value
    }

    /// `StatusBox` for the ACL answer.
    private final class AccessBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: GlomerisCredentialAccess?
        private var seconds: TimeInterval = 0

        func store(_ access: GlomerisCredentialAccess, after seconds: TimeInterval) {
            lock.lock()
            defer { lock.unlock() }
            stored = access
            self.seconds = seconds
        }

        var value: (access: GlomerisCredentialAccess, elapsed: TimeInterval)? {
            lock.lock()
            defer { lock.unlock() }
            guard let stored else { return nil }
            return (stored, seconds)
        }
    }

    /// `StatusBox` for a write outcome (HORO-1476).
    private final class WriteBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: GlomerisCredentialWriteOutcome?
        private var seconds: TimeInterval = 0

        func store(_ outcome: GlomerisCredentialWriteOutcome, after seconds: TimeInterval) {
            lock.lock()
            defer { lock.unlock() }
            stored = outcome
            self.seconds = seconds
        }

        var value: (outcome: GlomerisCredentialWriteOutcome, elapsed: TimeInterval)? {
            lock.lock()
            defer { lock.unlock() }
            guard let stored else { return nil }
            return (stored, seconds)
        }
    }

    /// Whether a `Task` has got to the end, for the one wait here that is meant
    /// to expire (HORO-1476).
    private final class CompletionFlag: @unchecked Sendable {
        private let lock = NSLock()
        private var finished = false

        func markFinished() {
            lock.lock()
            defer { lock.unlock() }
            finished = true
        }

        var isFinished: Bool {
            lock.lock()
            defer { lock.unlock() }
            return finished
        }
    }

    /// Runs one of the two write wrappers under a wall-clock bound (HORO-1476).
    ///
    /// The same shape as `resolveStatus(of:)` and for a stronger reason. These two
    /// have no deadline and are not getting one — what stops them is declining to
    /// dispatch at all — so removing that check does not make them slow, it makes
    /// them never return. `XCTWaiter` turns that into this named failure instead
    /// of an XCTest-level timeout with no explanation attached.
    private func performWrite(
        _ what: String,
        _ operation: @escaping @Sendable () async -> GlomerisCredentialWriteOutcome,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> (outcome: GlomerisCredentialWriteOutcome, elapsed: TimeInterval)? {
        let box = WriteBox()
        let returned = expectation(description: "\(what) returned")
        Task {
            let start = DispatchTime.now().uptimeNanoseconds
            let outcome = await operation()
            let seconds =
                Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000_000
            box.store(outcome, after: seconds)
            returned.fulfill()
        }
        let outcome = XCTWaiter.wait(for: [returned], timeout: Self.waitSeconds)
        guard outcome == .completed, let value = box.value else {
            XCTFail(
                """
                \(what) did not return within \(Self.waitSeconds)s. It has no \
                deadline and is not meant to have one: what makes it return when \
                the keychain has gone quiet is that it refuses to dispatch onto a \
                queue the stalled query is still holding.
                """,
                file: file, line: line)
            return nil
        }
        return value
    }

    // MARK: - HORO-1476: a write is not queued behind a stall

    /// The defect this ticket is about, on the save path. The availability query
    /// has already run out of time, so the work item it left behind holds the
    /// store's one serial keychain queue for the life of the process. A save
    /// dispatched onto that queue never runs — so it is not dispatched.
    ///
    /// Note what is asserted about the keychain: `writeCalls == 0`. Returning
    /// quickly would not be enough on its own, because a write that *did* reach a
    /// keychain and came back immediately looks the same from the outside.
    func testASaveIsNotIssuedOnceTheKeychainHasAlreadyGoneQuiet() {
        let (store, credentials, deadline) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)
        XCTAssertTrue(deadline.hasBeenMissed)

        guard let (outcome, elapsed) = performWrite(
            "resolvedSetApiKey", { await store.resolvedSetApiKey(Self.key) }
        ) else { return }

        XCTAssertEqual(outcome, .notAttempted)
        XCTAssertNotEqual(
            outcome, .refused,
            """
            A save that was never sent was reported as a refusal. That tells the \
            user macOS rejected their key and sends them to fix a permission that \
            is not broken, when what actually happened is that the keychain \
            service stopped answering.
            """)
        XCTAssertFalse(outcome.succeeded, "nothing was written, so this must not read as saved")
        XCTAssertEqual(
            credentials.writeCalls, 0,
            "the key was sent to a keychain that had already stopped answering")
        XCTAssertLessThan(
            elapsed, Self.deadlineSeconds / 3,
            "it waited, which means it was dispatched onto the held queue after all")
    }

    /// The same on the removal path, which matters for a different reason: a user
    /// pressing **Remove key** is usually revoking access, and needs to know the
    /// key is still there.
    func testARemovalIsNotIssuedOnceTheKeychainHasAlreadyGoneQuiet() {
        let (store, credentials, deadline) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)
        XCTAssertTrue(deadline.hasBeenMissed)

        guard let (outcome, elapsed) = performWrite(
            "resolvedDeleteApiKey", { await store.resolvedDeleteApiKey() }
        ) else { return }

        XCTAssertEqual(outcome, .notAttempted)
        XCTAssertFalse(
            outcome.succeeded,
            """
            A removal that was never sent was reported as done. The key is still \
            readable by everything that could read it before, and a user revoking \
            access would have been told the opposite.
            """)
        XCTAssertEqual(credentials.deleteCalls, 0)
        XCTAssertLessThan(elapsed, Self.deadlineSeconds / 3)
    }

    /// One record, not one per operation — the other direction of the pair above.
    /// A miss recorded by the ACL query stops the writes too, because what is
    /// wedged is the queue and not any one query.
    func testAMissRecordedByTheAccessListQueryAlsoStopsTheWrites() {
        let (store, credentials, _) = makeStalledStore()

        guard let first = resolveAccessList(of: store) else { return }
        XCTAssertEqual(first.access, .unresponsive)

        guard let save = performWrite(
            "resolvedSetApiKey", { await store.resolvedSetApiKey(Self.key) }
        ) else { return }
        guard let removal = performWrite(
            "resolvedDeleteApiKey", { await store.resolvedDeleteApiKey() }
        ) else { return }

        XCTAssertEqual(save.outcome, .notAttempted)
        XCTAssertEqual(removal.outcome, .notAttempted)
        XCTAssertEqual(credentials.writeCalls, 0)
        XCTAssertEqual(credentials.deleteCalls, 0)
    }

    // MARK: - HORO-1476 anti-vacuity: the writes still happen normally

    /// The control for the two tests above, and the one that makes
    /// `.notAttempted` mean something. The same double, allowed to answer, takes
    /// the save and reports it done — so `.notAttempted` came from the stall and
    /// not from a store that has stopped saving anything.
    ///
    /// Also the assertion that no deadline was added here by accident: a save
    /// against a working keychain must reach it, once.
    func testASaveStillReachesAKeychainThatIsAnswering() {
        let (store, credentials, deadline) = makeStalledStore()
        credentials.release()

        guard let (outcome, _) = performWrite(
            "resolvedSetApiKey", { await store.resolvedSetApiKey(Self.key) }
        ) else { return }

        XCTAssertEqual(outcome, .done)
        XCTAssertTrue(outcome.succeeded)
        XCTAssertEqual(credentials.writeCalls, 1, "the save must actually reach the keychain")
        XCTAssertFalse(
            deadline.hasBeenMissed,
            "a write must not record a miss: it has no deadline to miss")
    }

    func testARemovalStillReachesAKeychainThatIsAnswering() {
        let (store, credentials, deadline) = makeStalledStore()
        credentials.release()

        guard let (outcome, _) = performWrite(
            "resolvedDeleteApiKey", { await store.resolvedDeleteApiKey() }
        ) else { return }

        XCTAssertEqual(outcome, .done)
        XCTAssertEqual(credentials.deleteCalls, 1)
        XCTAssertFalse(deadline.hasBeenMissed)
    }

    // MARK: - HORO-1476: the boundary of this fix, pinned

    /// `resolvedChildEnvironment` still does not return when the keychain has gone
    /// quiet, and that is the scope boundary of this change rather than an
    /// oversight.
    ///
    /// It cannot be given the treatment above. Its return type is the child
    /// process's environment, so the only thing it could say in place of waiting
    /// is "no key" — and launching the CLI without a key the user has stored is
    /// the silently-wrong behaviour HORO-1471 argued against. Teaching it to
    /// report the wedge means changing what both of its callers do with the
    /// answer, which is the part of HORO-1476 that is a product decision.
    ///
    /// So this test asserts the current behaviour, on purpose, and it is meant to
    /// fail when that decision is taken. A failure here is the reminder to read
    /// HORO-1476 and update this file deliberately — not a regression.
    ///
    /// The parked work item is released by the teardown `makeStalledStore`
    /// registers: once the availability query is let through, the queued
    /// environment read runs immediately, because reading the secret from this
    /// double does not block.
    func testTheChildEnvironmentReadIsStillUnboundedAndThatIsDeliberate() {
        let (store, _, deadline) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)
        XCTAssertTrue(deadline.hasBeenMissed)

        // A flag and a poll rather than an XCTestExpectation, because this is the
        // one wait in this file that is *supposed* to time out. The read returns
        // once the teardown releases the gate, which is after this test has
        // finished, and fulfilling an expectation then is an XCTest API violation.
        let finished = CompletionFlag()
        Task {
            _ = await store.resolvedChildEnvironment(basedOn: [:])
            finished.markFinished()
        }

        let limit = Date().addingTimeInterval(Self.deadlineSeconds * 3)
        while !finished.isFinished && Date() < limit {
            Thread.sleep(forTimeInterval: 0.01)
        }

        XCTAssertFalse(
            finished.isFinished,
            """
            resolvedChildEnvironment returned while the keychain queue was held. \
            If that is because HORO-1476's remaining decision has been taken, this \
            test is the thing to change — deliberately, with the new behaviour \
            asserted in its place.
            """)
    }

    // MARK: - HORO-1474 AC 5: the ACL query is bounded too

    /// The ACL read gets the same treatment as the availability read, and for the
    /// same measured reason. Without the deadline this test does not fail, it
    /// hangs — which is exactly the state the AI Provider pane would be in.
    func testTheAccessListQueryIsBoundedByTheSameDeadline() {
        let (store, credentials, deadline) = makeStalledStore()

        guard let (access, elapsed) = resolveAccessList(of: store) else { return }

        XCTAssertEqual(access, .unresponsive)
        XCTAssertNotEqual(
            access, .applications([]),
            """
            Silence was reported as an empty trusted list. An empty list is the \
            strictest state an item can be in — nothing is pre-approved — so this \
            would tell the user their key is maximally protected at the moment the \
            app lost the ability to tell.
            """)
        XCTAssertGreaterThanOrEqual(
            elapsed, Self.deadlineSeconds * 0.8,
            "it answered without waiting, so the query never ran and this proves nothing")
        XCTAssertTrue(deadline.hasBeenMissed)
        XCTAssertEqual(credentials.accessListCalls, 1)
    }

    /// One record, not one per query. A miss recorded by the availability query
    /// means the ACL query is never issued at all — the stall is a single
    /// process-wide `dispatch_once` token, so there is nothing for a second query
    /// to find out, and issuing one would park a second thread on the pane the
    /// user opened to diagnose the first.
    func testAMissedAvailabilityDeadlineStopsTheAccessListQueryBeingIssued() {
        let (store, credentials, _) = makeStalledStore()

        guard let first = resolveStatus(of: store) else { return }
        XCTAssertEqual(first.status.apiKey, .unresponsive)

        guard let (access, elapsed) = resolveAccessList(of: store) else { return }

        XCTAssertEqual(access, .unresponsive)
        XCTAssertLessThan(
            elapsed, Self.deadlineSeconds / 3,
            "the ACL query waited out a deadline of its own instead of reading the record")
        XCTAssertEqual(
            credentials.accessListCalls, 0,
            "the keychain was asked for the ACL after it had already gone quiet")
    }

    /// And the other direction, because a deadline wired up in only one of the
    /// two places would pass the test above by accident.
    func testAMissedAccessListDeadlineStopsTheAvailabilityQueryBeingIssued() {
        let (store, credentials, _) = makeStalledStore()

        guard let first = resolveAccessList(of: store) else { return }
        XCTAssertEqual(first.access, .unresponsive)

        guard let (status, elapsed) = resolveStatus(of: store) else { return }

        XCTAssertEqual(status.apiKey, .unresponsive)
        XCTAssertLessThan(elapsed, Self.deadlineSeconds / 3)
        XCTAssertEqual(
            credentials.availabilityCalls, 0,
            "the availability query was issued after the ACL query had already gone quiet")
    }

    /// The control for the three above: the same double, allowed to answer,
    /// returns its list immediately. So `.unresponsive` came from the silence and
    /// not from a double that cannot report a list at all.
    func testTheSameDoubleReturnsItsAccessListOnceItIsAllowedTo() {
        let (store, credentials, deadline) = makeStalledStore()
        credentials.release()

        guard let (access, elapsed) = resolveAccessList(of: store) else { return }

        XCTAssertEqual(
            access,
            .applications([
                CredentialTrustedApplication(
                    reference: .resolves(path: "/Applications/Glomeris.app"), index: 0)
            ]))
        XCTAssertLessThan(elapsed, Self.deadlineSeconds / 3)
        XCTAssertFalse(deadline.hasBeenMissed)
        XCTAssertEqual(credentials.accessListCalls, 1)
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
