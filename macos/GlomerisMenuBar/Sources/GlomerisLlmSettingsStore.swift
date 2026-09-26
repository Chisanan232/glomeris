//
//  GlomerisLlmSettingsStore.swift
//  GlomerisMenuBar
//
//  HORO-1309: the BYOK provider settings — endpoint, model, API key — and the
//  rule for how they combine with the `GLOMERIS_LLM_*` environment variables
//  the Rust CLI already reads.
//
//  See the standing project rule in GlomerisMenuBarApp.swift. This file
//  decides nothing about providers: it does not validate the URL (the CLI's
//  `validate_base_url` does, and reports through `llm-check`), does not know
//  which models exist, and does not talk to a network. It persists three
//  values and assembles the child process's environment.
//

import Foundation

/// Where a single resolved setting came from — the answer to "why is this
/// working when I never typed it in", and equally to "why is it using the
/// wrong key".
///
/// Surfaced in the UI rather than kept internal, because a settings screen
/// that cannot explain an inherited value is how a user ends up editing a
/// field that has no effect.
enum GlomerisLlmSettingSource: Equatable, CaseIterable {
    /// Configured in this app: UserDefaults for the endpoint and model,
    /// the keychain for the key.
    case settings
    /// Not configured here, but present in the environment this app was
    /// launched with — so a `glomeris` child process would inherit it.
    case environment
    /// Not available from either place.
    case absent
    /// Stored here, but this app could not read it, and nothing was inherited
    /// to fall back to (HORO-1368). Only the API key can reach this state; the
    /// endpoint and model live in UserDefaults, which does not refuse.
    ///
    /// Distinct from `absent` because the remedy is different. "Not set" asks
    /// the user for a value they never provided; this asks them to provide a
    /// value they did, to a build that cannot get at the one they stored.
    case unreadable

    /// The keychain did not answer within the deadline, so whether a key is
    /// stored here is simply unknown (HORO-1471). Like `unreadable`, only the
    /// API key can reach this state.
    ///
    /// Distinct from `unreadable`, which is a *refusal*: an answer arrived and
    /// it was "no". This is the absence of any answer, and the remedies are
    /// opposite. A refusal is fixed by saving the key again under the running
    /// build's identity; a silence means the system's keychain service has
    /// stopped responding, and saving again would wait in the same place.
    ///
    /// Emphatically distinct from `absent`. "Not set" is a claim about the
    /// user's data, and making that claim because a system daemon went quiet is
    /// how a user gets told they have no key while their key sits in the
    /// keychain — and invited to paste it again into a store that cannot
    /// receive it.
    case unresponsive

    /// Whether a value will actually reach the CLI. Neither `unreadable` nor
    /// `unresponsive` will: nothing was inherited, and the stored item was
    /// either refused or never spoken about, so a spawned `glomeris` would see
    /// no variable and refuse with `NotConfigured`.
    var isAvailable: Bool {
        self == .settings || self == .environment
    }
}

/// Which binaries may read the stored key, as the settings screen sees it
/// (HORO-1474).
///
/// The store-level `CredentialAccessListReading` plus the one case a store
/// cannot produce, exactly as `GlomerisLlmSettingSource` extends
/// `CredentialAvailability` with `unresponsive`. A store has no deadline — it
/// issues a synchronous keychain call and either gets an answer or does not
/// return — so "the keychain did not say in time" can only be observed by the
/// layer that imposes the deadline, which is this one.
enum GlomerisCredentialAccess: Equatable {
    /// The applications trusted to read the key, in ACL order. An empty array
    /// means none is, which is the most restrictive state and not a failure.
    case applications([CredentialTrustedApplication])

    /// Nothing is stored under this app's identity, so there is no list.
    case noItem

    /// An item may exist, but the keychain would not say what may read it.
    case unreadable

    /// The keychain did not answer within the deadline (HORO-1471's rule,
    /// applied to this query too — AC 5).
    ///
    /// Distinct from `unreadable` for the same reason as on
    /// `GlomerisLlmSettingSource`: a refusal is an answer, this is silence. And
    /// emphatically distinct from `applications([])`, which is a claim that
    /// nothing is pre-trusted. Reporting silence as an empty list would tell a
    /// user their key is maximally protected at the moment the app stopped
    /// being able to tell.
    case unresponsive
}

/// One field's resolved state, without its value.
///
/// Deliberately value-free: this type is what the UI renders and what tests
/// assert on, and the API key is one of the three fields. A struct that
/// carried values would make "show the user where their key came from" and
/// "put the key on screen" the same operation.
struct GlomerisLlmSettingsStatus: Equatable {
    var endpoint: GlomerisLlmSettingSource
    var model: GlomerisLlmSettingSource

    /// `nil` while the answer is not known yet.
    ///
    /// Optional because of HORO-1368: this is the one field whose resolution
    /// touches the keychain, the keychain must not be touched on the main
    /// thread, and the screen has to render before an off-thread read can have
    /// finished. A placeholder case would have been the alternative, and it
    /// would have put "not configured yet" and "not looked at yet" into the
    /// same value — the first is a state to act on, the second is a spinner.
    var apiKey: GlomerisLlmSettingSource?

    /// Whether a live request could be attempted at all. All three are
    /// required — the CLI refuses with `NotConfigured` otherwise — so the UI
    /// can disable a connection-test button rather than invite a round trip
    /// that is guaranteed to fail locally.
    ///
    /// `false` while the key is still being resolved. A button that is briefly
    /// disabled is a smaller wrong than one that is briefly enabled.
    var isComplete: Bool {
        endpoint.isAvailable && model.isAvailable && apiKey?.isAvailable == true
    }
}

/// How long a keychain query may take before its answer is presumed not to be
/// coming, and the memory of that having happened (HORO-1471).
///
/// ## Why there is a memory at all
///
/// A deadline on its own is not enough, and the reason is specific rather than
/// defensive. The stall this exists for was traced into a one-time initialiser
/// inside the Security framework: the first caller to reach it takes a
/// `dispatch_once` token, sends a message to `securityd`, and waits. When no
/// reply ever comes the token is never released, so *every* later caller —
/// on any thread, any queue, in any part of the app — waits on the same token
/// for the life of the process. Timing out the first query therefore does not
/// restore the next one; it only stops the first from waiting forever.
///
/// So a missed deadline is recorded, and later queries are answered from the
/// record instead of being issued. That is the second half of HORO-1471 AC2 —
/// "a timed-out query is treated as terminal for the session and is not
/// re-issued" — chosen over the first half, isolating each query, because
/// isolation cannot help against a process-wide `dispatch_once`. Two isolated
/// queues would both stop at the same token.
///
/// ## What it costs
///
/// The abandoned work item is not cancelled, because a synchronous
/// `SecItemCopyMatching` cannot be cancelled. It keeps a thread parked for the
/// life of the process. That is the price of returning at all, and it is
/// bounded at one thread precisely because the miss is recorded and nothing
/// else is dispatched behind it.
///
/// A class rather than a value, because the record has to be shared by every
/// store instance in the process: the settings view constructs a new
/// `GlomerisLlmSettingsStore` on each re-evaluation, and a per-instance flag
/// would forget the miss immediately. Injectable so a test gets its own, since
/// a test that tripped the shared record would silently change what every later
/// test observes.
///
/// `@unchecked` for the mutable flag, which the lock below actually protects.
final class KeychainDeadline: @unchecked Sendable {
    /// The one the app uses. Deliberately shared, per the note above.
    static let shared = KeychainDeadline()

    /// Three seconds. An availability query is an attributes lookup with no
    /// decryption and no authorisation, which completes in well under a
    /// millisecond on a working system — measured in the low hundreds of
    /// microseconds — so this is not a performance budget, it is the point at
    /// which "slow" has stopped being a plausible explanation.
    ///
    /// Not shorter, because the cost of being wrong is asymmetric: a premature
    /// miss tells a user with a perfectly good keychain that it went quiet, and
    /// then keeps telling them that for the rest of the session. Not longer,
    /// because this is a settings pane a person is looking at.
    static let defaultSeconds: TimeInterval = 3

    let seconds: TimeInterval

    private let lock = NSLock()
    private var missed = false

    init(seconds: TimeInterval = KeychainDeadline.defaultSeconds) {
        self.seconds = seconds
    }

    /// Whether a query has already run out of time in this process.
    var hasBeenMissed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return missed
    }

    /// Records that one did. Not reversible: nothing this app can observe
    /// distinguishes "the daemon recovered" from "the token is still held", and
    /// a flag that cleared itself on a timer would re-park a thread per
    /// attempt.
    func recordMiss() {
        lock.lock()
        defer { lock.unlock() }
        missed = true
    }
}

/// A continuation that two places may try to resume, and that resumes once.
///
/// Needed because the deadline and the work are genuinely racing, and resuming
/// a checked continuation twice is a crash rather than a warning. The loser's
/// call is dropped.
private final class FirstAnswer<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Value?, Never>?

    /// Called synchronously inside `withCheckedContinuation`'s closure, before
    /// anything that could deliver is dispatched — so there is no window in
    /// which an answer arrives with nowhere to go.
    func attach(_ continuation: CheckedContinuation<Value?, Never>) {
        lock.lock()
        defer { lock.unlock() }
        self.continuation = continuation
    }

    func deliver(_ value: Value?) {
        lock.lock()
        let waiting = continuation
        continuation = nil
        lock.unlock()
        // Outside the lock: `resume` hands control to the awaiting task, and
        // holding a lock across that is how a deadlock gets written.
        waiting?.resume(returning: value)
    }
}

/// Reads and writes the BYOK provider settings, and builds the environment a
/// `glomeris` child process should run with.
///
/// ## Precedence
///
/// Per field, what this app has configured wins over what it inherited. The
/// rule is "the settings screen never lies": if the endpoint field shows a
/// URL, that is the URL used. A field left empty here falls back to the
/// inherited `GLOMERIS_LLM_*` variable, which is what keeps a user who
/// already exported them in their shell — and launches the app from that
/// shell — working without retyping anything (AC 3).
///
/// The alternative, environment-wins, was rejected: it makes the settings
/// screen display one endpoint while silently using another, and gives the
/// user no way to correct it from the GUI.
///
/// The CLI is unaffected either way. `glomeris` in a terminal reads that
/// terminal's environment and knows nothing about this store.
///
/// ## Where the key goes
///
/// Into the child's environment, and nowhere else. Never a command-line
/// argument (visible in `ps` and refused by the CLI by flag name), never
/// UserDefaults, never a file this app writes, never a log line. It is read
/// from the keychain at the moment a command is spawned and not cached.
///
/// ## Threads
///
/// Everything here is safe to call from any thread, and the two members that
/// reach the keychain must be called off the main one — see
/// [`resolvedStatus(environment:)`].
///
/// `@unchecked` for exactly one reason: `UserDefaults` is not marked `Sendable`
/// by Foundation, while being documented as thread-safe. Everything else here
/// is — `CredentialStore` requires it, and this struct has no mutable state of
/// its own. The unchecked part is that one documented guarantee and nothing
/// else.
struct GlomerisLlmSettingsStore: @unchecked Sendable {
    private static let endpointKey = "llmBaseUrl"
    private static let modelKey = "llmModel"

    /// The keychain account name for the API key. Not a UserDefaults key —
    /// nothing under this name is ever written to UserDefaults.
    static let apiKeyAccount = "llmApiKey"

    static let endpointEnvironmentVariable = "GLOMERIS_LLM_BASE_URL"
    static let modelEnvironmentVariable = "GLOMERIS_LLM_MODEL"
    static let apiKeyEnvironmentVariable = "GLOMERIS_LLM_API_KEY"

    private let defaults: UserDefaults
    private let credentials: CredentialStore
    private let keychainDeadline: KeychainDeadline

    /// `UserDefaults.standard` is the default rather than a named suite: for a
    /// bundled app it *is* the domain named by its bundle identifier, so the
    /// endpoint and model are namespaced under the running bundle by
    /// construction, and a build with a different identifier reads its own
    /// settings rather than the release app's.
    ///
    /// HORO-1456: this was `UserDefaults(suiteName:) ?? .standard` against the
    /// literal `dev.glomeris.GlomerisMenuBar`. Foundation returns `nil` from
    /// that initialiser when handed the calling process's own bundle
    /// identifier, so in the app the fallback was always the branch taken —
    /// the storage is unchanged, and nothing needs migrating.
    ///
    /// `keychainDeadline` defaults to the shared one rather than to a fresh one
    /// on purpose (HORO-1471): a new store is constructed on every re-evaluation
    /// of the settings view, and a per-instance record of a missed deadline
    /// would be forgotten before it could stop the next query.
    init(
        defaults: UserDefaults? = nil,
        credentials: CredentialStore? = nil,
        keychainDeadline: KeychainDeadline = .shared
    ) {
        self.defaults = defaults ?? .standard
        self.credentials = credentials ?? KeychainCredentialStore()
        self.keychainDeadline = keychainDeadline
    }

    // MARK: - Stored values

    /// The configured endpoint, or `nil` if the field is empty. Whitespace-only
    /// is `nil` too: a user who selects a value and hits delete has cleared
    /// the field, whatever residue the text view reports.
    var endpoint: String? {
        get { Self.normalized(defaults.string(forKey: Self.endpointKey)) }
        nonmutating set { Self.write(newValue, to: Self.endpointKey, in: defaults) }
    }

    var model: String? {
        get { Self.normalized(defaults.string(forKey: Self.modelKey)) }
        nonmutating set { Self.write(newValue, to: Self.modelKey, in: defaults) }
    }

    /// Whether an API key is stored here, and whether that could be
    /// established at all. There is deliberately no getter for the key itself
    /// on this type: the only code that needs the value is
    /// [`childEnvironment(basedOn:)`], which puts it straight into a child
    /// process's environment. Anything else asking for it would be a leak in
    /// the making.
    ///
    /// Asks the store for *availability* and not for the value (HORO-1368).
    /// Retrieving the value can block behind an authorisation prompt; asking
    /// whether an item exists cannot.
    var apiKeyAvailability: CredentialAvailability {
        credentials.availability(forKey: Self.apiKeyAccount)
    }

    /// Whether an API key is stored here.
    ///
    /// `false` when the keychain refused the question, which is the same answer
    /// this gave before HORO-1368 and is why the UI uses
    /// [`status(environment:)`] instead: a `Bool` cannot tell "no key" apart
    /// from "a key this build cannot reach".
    var hasStoredApiKey: Bool {
        apiKeyAvailability == .present
    }

    /// Stores the API key, trimmed — a pasted key routinely carries a trailing
    /// newline, and a key with one is a 401 nobody can explain by looking at
    /// the field.
    ///
    /// Storing an empty string deletes instead, so "clear the field and save"
    /// and "revoke" cannot disagree about what happened.
    @discardableResult
    func setApiKey(_ key: String) -> Bool {
        guard let trimmed = Self.normalized(key) else { return deleteApiKey() }
        return credentials.setSecret(trimmed, forKey: Self.apiKeyAccount)
    }

    /// Removes the stored key (AC 8). Succeeds when no key is stored
    /// afterwards, including when none was stored before.
    @discardableResult
    func deleteApiKey() -> Bool {
        credentials.deleteSecret(forKey: Self.apiKeyAccount)
    }

    /// [`setApiKey(_:)`] off the main thread.
    ///
    /// Writes block for the same reason reads do: `SecItemUpdate` replaces an
    /// existing item's data, and the item's ACL guards that too, so saving over
    /// a key stored by a different build can wait on an authorisation prompt
    /// (HORO-1368 AC 1).
    @discardableResult
    func resolvedSetApiKey(_ key: String) async -> Bool {
        await Self.offMainThread { self.setApiKey(key) }
    }

    /// [`deleteApiKey()`] off the main thread, for the same reason.
    @discardableResult
    func resolvedDeleteApiKey() async -> Bool {
        await Self.offMainThread { self.deleteApiKey() }
    }

    // MARK: - Resolution

    /// Where each field's effective value comes from, given `environment`
    /// (the launching environment, injectable for tests).
    ///
    /// Touches the keychain, so it must not be called on the main thread — use
    /// [`resolvedStatus(environment:)`].
    ///
    /// The read asks for availability and not for the value, so it cannot raise
    /// an authorisation prompt and cannot wait on a person. This used to be
    /// written down as the read being "non-blocking", which is a different and
    /// false claim, and HORO-1471 is what it cost: it is a synchronous call into
    /// `securityd`, it has no bound of its own, and on a Mac where that daemon
    /// stops replying it never returns. That is why the async wrapper, and not
    /// this function, is the one the app calls — only the wrapper has a
    /// deadline. This one is kept for tests and for callers that are already off
    /// the main thread and want the raw answer.
    func status(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> GlomerisLlmSettingsStatus {
        var status = statusWithoutApiKey(environment: environment)
        status.apiKey = Self.source(
            availability: apiKeyAvailability,
            inherited: environment[Self.apiKeyEnvironmentVariable])
        return status
    }

    /// The endpoint and model only, with `apiKey` left `nil`.
    ///
    /// Reaches UserDefaults and nothing else, so it is safe on the main thread
    /// and is what a view seeds itself with (HORO-1368 AC 2). The two fields it
    /// does resolve are the two a user edits by typing, and re-resolving them on
    /// every keystroke must not imply a keychain round trip per character.
    func statusWithoutApiKey(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> GlomerisLlmSettingsStatus {
        GlomerisLlmSettingsStatus(
            endpoint: Self.source(
                stored: endpoint != nil,
                inherited: environment[Self.endpointEnvironmentVariable]),
            model: Self.source(
                stored: model != nil,
                inherited: environment[Self.modelEnvironmentVariable]),
            apiKey: nil
        )
    }

    /// What [`status(environment:)`] answers, obtained off the main thread and,
    /// for the one field that needs the keychain, under a deadline.
    ///
    /// The point of the hop is not speed. A keychain operation can wait on a
    /// person — `SecurityAgent` puts up an authorisation prompt and returns
    /// nothing until it is answered — and a main thread that is waiting on a
    /// person cannot service the status item, so AppKit removes it and the app
    /// disappears from the menu bar with no way back in (HORO-1368).
    ///
    /// The point of the deadline is different, and is HORO-1471: waiting off the
    /// main thread is only bounded if the thing being waited on answers. The
    /// endpoint and the model are resolved out here rather than inside the hop
    /// because they reach UserDefaults and nothing else — putting them behind
    /// the keychain query is what let a silent keychain leave the whole pane
    /// unresolved instead of just the one row it actually concerns.
    func resolvedStatus(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) async -> GlomerisLlmSettingsStatus {
        var status = statusWithoutApiKey(environment: environment)
        status.apiKey = await resolvedApiKeySource(environment: environment)
        return status
    }

    /// Where the API key comes from, or `unresponsive` if the keychain did not
    /// say in time (HORO-1471).
    ///
    /// The deadline is not a retry. Once one query has run out of time the
    /// answer is served from the record and the keychain is not asked again,
    /// because the stall is a process-wide `dispatch_once` and a second attempt
    /// would wait out a second deadline to be told the same thing — see
    /// `KeychainDeadline`. This is AC2's "terminal for the session".
    ///
    /// An inherited `GLOMERIS_LLM_API_KEY` still outranks the silence, exactly
    /// as it outranks a refusal: the child process gets the inherited key and
    /// the user has nothing to fix, so there is nothing to report.
    private func resolvedApiKeySource(
        environment: [String: String]
    ) async -> GlomerisLlmSettingSource {
        let inherited = environment[Self.apiKeyEnvironmentVariable]

        if keychainDeadline.hasBeenMissed {
            return Self.source(availability: nil, inherited: inherited)
        }

        let availability = await Self.offMainThread(within: keychainDeadline.seconds) {
            self.apiKeyAvailability
        }
        if availability == nil { keychainDeadline.recordMiss() }
        return Self.source(availability: availability, inherited: inherited)
    }

    /// Which binaries may read the stored key, without reading it (HORO-1474).
    ///
    /// Touches the keychain, so it must not be called on the main thread — use
    /// [`resolvedAccessList()`]. Same hazard as [`apiKeyAvailability`], for a
    /// narrower reason: this one asks for a reference and never for data, so it
    /// cannot raise an authorisation prompt at all, but it is still a synchronous
    /// call into `securityd` with no bound of its own.
    var apiKeyAccessList: CredentialAccessListReading {
        credentials.accessList(forKey: Self.apiKeyAccount)
    }

    /// [`apiKeyAccessList`] off the main thread and under the deadline (AC 5).
    ///
    /// The same deadline object as the availability query, not a second one of
    /// its own, and that is the whole point. The stall is one process-wide
    /// `dispatch_once` token inside the Security framework — see
    /// `KeychainDeadline` — so a query that has already timed out anywhere has
    /// established that *this* query will too. A private deadline here would
    /// park a second thread to rediscover it, and would do so on the pane the
    /// user opened to find out what went wrong.
    func resolvedAccessList() async -> GlomerisCredentialAccess {
        if keychainDeadline.hasBeenMissed { return .unresponsive }

        let reading = await Self.offMainThread(within: keychainDeadline.seconds) {
            self.apiKeyAccessList
        }
        guard let reading else {
            keychainDeadline.recordMiss()
            return .unresponsive
        }

        switch reading {
        case .applications(let applications): return .applications(applications)
        case .noItem: return .noItem
        case .unreadable: return .unreadable
        }
    }

    /// [`childEnvironment(basedOn:)`] performed off the main thread.
    ///
    /// This is the call that reads the key itself, so it is the one that really
    /// can block on a prompt rather than merely being able to in principle
    /// (AC 3). Callers are already `async` — both are button actions that then
    /// spawn a `glomeris` child — so the hop costs them nothing.
    func resolvedChildEnvironment(
        basedOn environment: [String: String] = ProcessInfo.processInfo.environment
    ) async -> [String: String] {
        await Self.offMainThread { self.childEnvironment(basedOn: environment) }
    }

    /// The full environment to launch a `glomeris` child process with.
    ///
    /// Returns a *complete* environment, not an overlay of the three
    /// variables, because `Process.environment` is a wholesale replacement:
    /// assigning three variables to it would leave the child with no `PATH`,
    /// no `HOME` and no `TMPDIR`, which breaks executable resolution and every
    /// HOME-bounded detector. So this starts from `environment` and sets only
    /// what this app has configured.
    ///
    /// A field configured here overrides the inherited variable; a field left
    /// empty leaves the inherited one untouched, including leaving it absent.
    /// Nothing is ever removed — a user clearing a GUI field falls back to the
    /// environment rather than having a working setup switched off by a UI
    /// they may not have meant to change.
    func childEnvironment(
        basedOn environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> [String: String] {
        var result = environment
        if let endpoint { result[Self.endpointEnvironmentVariable] = endpoint }
        if let model { result[Self.modelEnvironmentVariable] = model }
        // Read at spawn time and not retained anywhere: the returned
        // dictionary is handed to `Process` and dropped.
        if let key = credentials.secret(forKey: Self.apiKeyAccount) {
            result[Self.apiKeyEnvironmentVariable] = key
        }
        return result
    }

    // MARK: - Helpers

    private static func source(stored: Bool, inherited: String?) -> GlomerisLlmSettingSource {
        if stored { return .settings }
        if normalized(inherited) != nil { return .environment }
        return .absent
    }

    /// The same rule for the one field whose storage can refuse the question —
    /// or, since HORO-1471, not answer it at all.
    ///
    /// `nil` availability means no answer arrived before the deadline. It is
    /// deliberately not `unreadable`: that is a refusal, which is an answer.
    ///
    /// An inherited variable still wins over either, and does so silently,
    /// because in that case the app genuinely does work: the child process gets
    /// the inherited key and the user has nothing to fix. The degraded state is
    /// reported only when there is no fallback — which is exactly when it
    /// changes what the user can do.
    private static func source(
        availability: CredentialAvailability?,
        inherited: String?
    ) -> GlomerisLlmSettingSource {
        if availability == .present { return .settings }
        if normalized(inherited) != nil { return .environment }
        switch availability {
        case .unreadable: return .unreadable
        // No answer. Anything else here — `absent` in particular — would be
        // this app stating something about the user's data that it does not
        // know (AC1).
        case nil: return .unresponsive
        default: return .absent
        }
    }

    /// Keychain work, off the main thread and one at a time.
    ///
    /// Two separate properties, and only the second one needs this queue —
    /// measured rather than assumed. Awaiting a nonisolated `async` function from
    /// a main-actor caller already leaves the main actor, so replacing the body
    /// below with a bare `work()` still runs the keychain call off the main
    /// thread. What it loses is serialisation: two concurrent reads of the same
    /// ACL-guarded item can raise two authorisation prompts, and a user facing a
    /// stack of identical prompts cannot tell whether answering one did anything.
    ///
    /// The explicit queue is kept for that, and because "off the main thread"
    /// then stops depending on a caller remembering to `await` from somewhere
    /// nonisolated. `MainActor.run` here — the one-line change that reintroduces
    /// the bug — is what the off-thread tests actually catch.
    ///
    /// The label is derived rather than written out (HORO-1456). A queue label
    /// is diagnostic only — it namespaces nothing and no state is filed under
    /// it — but it was the last place in `Sources/` spelling the release bundle
    /// identifier out, and leaving it would have meant the guard script that
    /// keeps that literal from coming back needed an exemption list. An
    /// exemption list is where the next literal hides.
    private static let keychainQueue = DispatchQueue(
        label: "\(BundleIdentity.current).keychain",
        qos: .userInitiated
    )

    private static func offMainThread<T: Sendable>(
        _ work: @escaping @Sendable () -> T
    ) async -> T {
        await withCheckedContinuation { continuation in
            keychainQueue.async { continuation.resume(returning: work()) }
        }
    }

    /// Where the deadline is timed.
    ///
    /// Load-bearing that this is not `keychainQueue`. The case the deadline
    /// exists for is a work item on `keychainQueue` that never finishes, and a
    /// serial queue holding a stuck item runs nothing else — so a deadline
    /// scheduled there would be queued behind the very stall it is meant to
    /// break, and would never fire. The one-line change that reintroduces
    /// HORO-1471 is moving this `asyncAfter` onto the keychain queue.
    private static let deadlineQueue = DispatchQueue(
        label: "\(BundleIdentity.current).keychain.deadline",
        qos: .userInitiated
    )

    /// [`offMainThread(_:)`] with a deadline. `nil` means the deadline passed
    /// first; it does not mean the work produced nothing.
    ///
    /// The work item is still running when `nil` is returned — see
    /// `KeychainDeadline` for why it cannot be cancelled and why that is
    /// survivable.
    private static func offMainThread<T: Sendable>(
        within seconds: TimeInterval,
        _ work: @escaping @Sendable () -> T
    ) async -> T? {
        let answer = FirstAnswer<T>()
        return await withCheckedContinuation { continuation in
            answer.attach(continuation)
            keychainQueue.async { answer.deliver(work()) }
            deadlineQueue.asyncAfter(deadline: .now() + seconds) { answer.deliver(nil) }
        }
    }

    /// `nil` for absent, empty, or whitespace-only. One rule, applied to
    /// stored and inherited values alike, so an exported-but-empty
    /// `GLOMERIS_LLM_MODEL=` reads as absent here exactly as it does in the
    /// Rust `provider_from_parts`.
    private static func normalized(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
            !trimmed.isEmpty
        else { return nil }
        return trimmed
    }

    private static func write(_ value: String?, to key: String, in defaults: UserDefaults) {
        if let normalized = normalized(value) {
            defaults.set(normalized, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}
