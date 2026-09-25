//
//  CredentialStore.swift
//  GlomerisMenuBar
//
//  HORO-1309: where the BYOK LLM API key lives. The key is the only secret
//  this app ever holds, so it gets the only storage in the app that is not
//  UserDefaults.
//
//  See the standing project rule in GlomerisMenuBarApp.swift — this file
//  stores and retrieves an opaque string. It makes no policy, evidence or
//  action decision, and it never decides what the key is *for*; the Rust CLI
//  owns every rule about the provider, and the key reaches it only as an
//  environment variable on the child process.
//

import Foundation
import Security

/// Whether a secret is stored, answered without reading it.
///
/// The distinction from `secret(forKey:) != nil` is the whole point of
/// HORO-1368. Retrieving the value requires the keychain to *decrypt* the
/// item, which requires the calling binary's code identity to be listed in the
/// item's ACL — and when it is not, `SecItemCopyMatching` does not fail, it
/// blocks behind a `SecurityAgent` authorisation prompt for as long as the
/// prompt is unanswered. Asking only whether an item exists needs no
/// authorisation, so it cannot prompt and cannot block on a person.
///
/// `unreadable` is kept apart from `absent` because the two mean opposite
/// things to a user: nothing is configured, versus something is configured and
/// this app cannot get at it. Collapsing them is how the settings screen ends
/// up saying "Not set" about a key that is very much set, and inviting the user
/// to paste a key they already pasted.
enum CredentialAvailability: Equatable {
    /// An item exists for this key.
    case present
    /// No item exists for this key.
    case absent
    /// An item may exist, but the keychain would not say — it refused the
    /// query for some reason other than the item being missing.
    case unreadable
}

/// Reading and writing one secret, behind a protocol.
///
/// The protocol exists for testability, and specifically because the
/// alternative was a test suite that silently proves nothing: an unsigned
/// `xcodebuild` test binary has no keychain-access-group entitlement, so
/// `SecItemAdd` can fail with `errSecMissingEntitlement` (-34018) in CI while
/// passing on a developer machine. A test that skipped itself on that error
/// would be a permanently green no-op — the HORO-1253 failure mode. So the
/// behaviour (`GlomerisLlmSettingsStore`'s precedence, delete semantics, what
/// reaches the child process) is tested against `InMemoryCredentialStore`,
/// and the real Keychain boundary is covered by a mechanical source check
/// (`scripts/check-credential-store-uses-keychain.sh`) plus
/// `KeychainCredentialStore`'s own tests, which assert the *outcome they
/// observe* rather than requiring a particular one.
/// `Sendable` because HORO-1368 moves every call off the main thread: a
/// keychain operation is I/O that can wait on a person, and the main thread
/// cannot wait on a person without the app's only menu-bar item disappearing.
protocol CredentialStore: Sendable {
    /// Whether a secret is stored for `key`, without retrieving it.
    ///
    /// Prefer this to `secret(forKey:) != nil` everywhere the value is not
    /// actually needed — which is everywhere except building a child process's
    /// environment. See `CredentialAvailability`.
    func availability(forKey key: String) -> CredentialAvailability

    /// The stored secret, or `nil` if there is none. Returns `nil` rather
    /// than throwing on a keychain failure: no caller can do anything
    /// different for "absent" versus "unreadable", and the difference is not
    /// worth an error path that might end up rendering a message containing
    /// the item it failed to read.
    func secret(forKey key: String) -> String?

    /// Stores `secret`, replacing any existing value for `key`. Returns
    /// whether it succeeded, because this one *is* worth telling the user
    /// about: silently failing to save a key they just pasted would leave
    /// them re-pasting it forever.
    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool

    /// Removes the stored secret. Returns `true` if the key is absent
    /// afterwards — so deleting something that was never there succeeds,
    /// which is what "make sure this is gone" should mean.
    @discardableResult
    func deleteSecret(forKey key: String) -> Bool
}

/// The real store: a generic-password keychain item in the user's login
/// keychain.
///
/// `kSecClassGenericPassword` with a fixed service and the caller's key as the
/// account, which is the plainest shape that works without an entitlement on a
/// signed app and keeps the item visible and revocable in Keychain Access —
/// deliberately, because AC 8 wants revocation to be possible outside this app
/// too, not only through a button this app must be running to offer.
///
/// Every operation is synchronous and touches only this service's items.
/// Nothing here logs, prints, or returns a description of a failure that
/// includes the secret: the `OSStatus` is deliberately discarded rather than
/// surfaced, because a status code cannot help a user and a message built
/// around the item risks carrying it.
struct KeychainCredentialStore: CredentialStore {
    /// The running bundle's identifier, which is also the domain
    /// `UserDefaults.standard` writes to, so all of this app's stored state is
    /// findable under one name — and under a *different* one in a build with a
    /// different identifier.
    ///
    /// HORO-1456: this was the literal `dev.glomeris.GlomerisMenuBar`. Equal to
    /// the release identifier, so no shipped build changes behaviour here and
    /// no stored key needs migrating; the difference is that a diagnostic or
    /// beta build now gets its own keychain items instead of the user's, with
    /// no source file to remember to patch. See `BundleIdentity` for what this
    /// resolves to in a test process, where it is deliberately neither the
    /// release identifier nor a crash.
    static let service = BundleIdentity.current

    private let service: String

    init(service: String = KeychainCredentialStore.service) {
        self.service = service
    }

    /// Shown by `SecurityAgent` when it asks the user about this item, so it
    /// should read as a sentence a person can act on.
    static let accessDescription = "Glomeris LLM provider key"

    /// A `SecAccess` trusting exactly the running binary and nothing else.
    ///
    /// HORO-1455 AC 2, and it is an intent pin rather than a hardening change —
    /// stated plainly because the opposite is the easy thing to believe.
    /// Measured on a scratch keychain, `SecItemAdd` with no `kSecAttrAccess`,
    /// with `SecAccessCreate(desc, nil)`, and with `SecAccessCreate(desc,
    /// [self])` all produce the *same* ACL: three entries, the decrypt entry
    /// trusting one application. The default was already self-only, so this
    /// grants nothing and revokes nothing.
    ///
    /// What it buys is that the trusted list is now written down where a change
    /// to it has to be deliberate. `scripts/check-credential-store-uses-keychain.sh`
    /// and `CredentialStoreAccessTests` both assert this call is here, so a
    /// later commit that adds a second application to the list, or that passes
    /// `nil` for the applications — which means "no ACL restriction", not "the
    /// default" — fails a check instead of quietly shipping an item any binary
    /// can read.
    ///
    /// Returning `nil` on failure is deliberate, and `insertAttributes` treats it
    /// as "omit the key" rather than as an error: an item created with the
    /// default ACL is byte-for-byte what HORO-1309 shipped, so refusing to save
    /// the user's key over it would turn a cosmetic failure into a visible one.
    ///
    /// Both calls below are deprecated (10.10, "SecKeychain is deprecated") and
    /// the two warnings are left in place on purpose. They are not noise: they
    /// are the compiler stating the same thing HORO-1455 asks the founder to
    /// decide — that Apple's supported home for a secret is the data-protection
    /// keychain, which needs an entitlement this app does not have. A file
    /// keychain item has no other ACL mechanism, so there is no non-deprecated
    /// way to write this while the file keychain is the choice. Silencing them
    /// (by marking this method deprecated too, which does suppress them) would
    /// only move the warning to the call site and would hide the one signal that
    /// says which decision is outstanding.
    static func selfOnlyAccess() -> SecAccess? {
        var trustedSelf: SecTrustedApplication?
        // A nil path means "the application making this call", resolved from the
        // running code's identity. Correct across a rename or a move, which a
        // hardcoded path would not be.
        guard SecTrustedApplicationCreateFromPath(nil, &trustedSelf) == errSecSuccess,
            let trustedSelf
        else {
            return nil
        }

        var access: SecAccess?
        guard
            SecAccessCreate(
                accessDescription as CFString, [trustedSelf] as CFArray, &access) == errSecSuccess
        else {
            return nil
        }
        return access
    }

    private func baseQuery(forKey key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
    }

    /// Asks for the item's attributes and never for its data: with no
    /// `kSecReturnData` there is nothing to decrypt, so the item's ACL is not
    /// consulted and no authorisation prompt can appear. The returned
    /// attributes are discarded unread — the answer is the `OSStatus`.
    ///
    /// A return key must be requested explicitly. `SecItemCopyMatching` with no
    /// `kSecReturn*` key at all defaults to returning data for a generic
    /// password, which is exactly the prompting call this exists to avoid.
    ///
    /// This used to be described as the store's *non-blocking* half. It is not,
    /// and HORO-1471 is what that cost. Not prompting means it cannot block on
    /// a *person*, which is the only claim the paragraph above supports. It is
    /// still a synchronous request to another process, and on a Mac where
    /// `securityd` accepts the connection and then never replies — measured on
    /// one, not hypothesised — this call does not return, ever. It cannot be
    /// cancelled either: the thread is parked inside the Security framework.
    /// Callers must therefore treat it as unbounded and impose their own
    /// deadline; `KeychainDeadline` in `GlomerisLlmSettingsStore.swift` is the
    /// one the settings screen uses.
    func availability(forKey key: String) -> CredentialAvailability {
        var query = baseQuery(forKey: key)
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var attributes: CFTypeRef?
        switch SecItemCopyMatching(query as CFDictionary, &attributes) {
        case errSecSuccess: return .present
        case errSecItemNotFound: return .absent
        // Everything else — a locked keychain, a missing entitlement on an
        // unsigned build (-34018), a corrupt item. The status is deliberately
        // not surfaced: it cannot help a user, and HORO-1309's rule is that no
        // message is built around an item this app failed to read.
        default: return .unreadable
        }
    }

    func secret(forKey key: String) -> String? {
        var query = baseQuery(forKey: key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
            let data = item as? Data,
            let secret = String(data: data, encoding: .utf8),
            !secret.isEmpty
        else {
            return nil
        }
        return secret
    }

    /// The attributes a freshly created item is given.
    ///
    /// Split out of `setSecret` so it can be asserted on without a keychain.
    /// `SecItemAdd` itself cannot run under test: an unsigned `xcodebuild` test
    /// binary writing to the login keychain gets `errSecAuthFailed` (-25293),
    /// and one asking for the data-protection keychain gets
    /// `errSecMissingEntitlement` (-34018). A test that skipped itself on either
    /// status would be the HORO-1253 failure mode — a gate that never ran
    /// reading as a gate that passed. So the dictionary is built here, where a
    /// test can look at exactly what would have been handed to the keychain, and
    /// `setSecret` adds nothing to it.
    func insertAttributes(forKey key: String, data: Data) -> [String: Any] {
        var insert = baseQuery(forKey: key)
        insert[kSecValueData as String] = data
        // HORO-1455 AC 1. This is a *file* keychain item — the login keychain —
        // and `kSecAttrAccessible` is honoured by the data-protection keychain,
        // not by a file keychain. Reaching the data-protection keychain needs an
        // entitlement this app does not have: measured, an unsigned build passing
        // `kSecUseDataProtectionKeychain` gets errSecMissingEntitlement (-34018).
        // So the attribute is stored and inert.
        //
        // It is set anyway, because it is the correct value to already be
        // carrying if Glomeris becomes a signed, entitled app, and because
        // removing it would read as a decision to allow syncing. But the comment
        // that used to be here claimed the protection it describes was in force,
        // and two thirds of that claim were false. What actually holds today:
        //
        //   * Encrypted at rest while the login keychain is locked. True, and it
        //     is the login keychain's doing, not this attribute's.
        //   * Not synced to iCloud. True, but because a file keychain is not
        //     syncable at all — `kSecAttrSynchronizable` is a data-protection
        //     attribute too.
        //   * Not included in a backup. FALSE. ~/Library/Keychains is an
        //     ordinary directory, so any file-level backup of the home directory
        //     contains the item. It is still encrypted there, and useless
        //     without the keychain password.
        //
        // Whether to move to the data-protection keychain and pay its costs is a
        // founder call recorded on HORO-1455, not something to decide in a
        // comment. See book/src/byok.md, which now says the same thing to a user.
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        // HORO-1455 AC 2. Trust this binary and nothing else. Produces the same
        // ACL the default does — see `selfOnlyAccess()`; the point is that the
        // list is declared rather than inherited.
        if let access = Self.selfOnlyAccess() {
            insert[kSecAttrAccess as String] = access
        }
        return insert
    }

    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool {
        guard let data = secret.data(using: .utf8) else { return false }

        // HORO-1455 AC 3. Delete-then-add, where this used to update-then-add.
        // The old comment wanted an existing item "replaced in place so its
        // access control ... survive[s]", and surviving access control is the
        // defect: every "Always Allow" the user answers for a superseded build
        // adds that build to the item's trusted list, and nothing ever takes it
        // off again. After enough upgrades the key is readable by every past
        // Glomeris binary on the disk.
        //
        // Measured on a scratch keychain, which is why this is delete-then-add
        // and not something more surgical:
        //
        //   * `SecItemUpdate` with the value alone leaves the ACL untouched — a
        //     list deliberately widened to two applications was still two
        //     afterwards. That is the accretion, reproduced.
        //   * `SecItemUpdate` carrying `kSecAttrAccess` does not narrow the list.
        //     It blocks indefinitely, even with user interaction disabled, so it
        //     cannot even fail usefully. An ACL cannot be rewritten in place.
        //   * Creating a fresh item resets the decrypt entry to one trusted
        //     application — and does so whether or not an access specification
        //     is supplied, so the reset comes from the add, not from AC 2.
        //
        // This does not remove the upgrade prompt, and claiming it did would be
        // wrong: a binary absent from the item's trusted list cannot delete it
        // either. Measured, that delete returns errSecInvalidOwnerEdit (-25244)
        // with interaction disabled and the item survives, which is the same
        // authorisation a read needs. What it removes is the *permanence* —
        // answering the prompt once replaces the widened item with a fresh
        // one-application ACL, instead of appending to a list that only grows.
        let deleted = SecItemDelete(baseQuery(forKey: key) as CFDictionary)
        guard deleted == errSecSuccess || deleted == errSecItemNotFound else {
            // The user declined the authorisation, or it failed. Report the save
            // as failed rather than falling back to an update: the fallback is
            // exactly the accretion above, and it would make the fix conditional
            // on the user never pressing Deny.
            return false
        }

        // The inverse of the old comment's other claim, and the honest cost of
        // this change: a failed add now leaves no key at all, where before the
        // old one survived. That is the trade AC 3 asks for, and it is the safer
        // direction — the failure the user sees is "paste it again", not "your
        // key is still readable by a binary you stopped trusting".
        return SecItemAdd(insertAttributes(forKey: key, data: data) as CFDictionary, nil)
            == errSecSuccess
    }

    @discardableResult
    func deleteSecret(forKey key: String) -> Bool {
        let status = SecItemDelete(baseQuery(forKey: key) as CFDictionary)
        // `errSecItemNotFound` is success: the postcondition is "no secret is
        // stored for this key", and that already holds.
        return status == errSecSuccess || status == errSecItemNotFound
    }
}

/// A `CredentialStore` held in memory, for tests.
///
/// A class rather than a struct because the protocol's mutating operations are
/// non-mutating (the real store mutates the keychain, not itself), so a value
/// type could not record anything. Not used in production — the source guard
/// asserts the settings store is constructed with the keychain-backed one.
///
/// `@unchecked` because the dictionary is mutable and the protocol is
/// `Sendable`; the lock below is what actually holds the guarantee up, and it is
/// there for a real reason. Since HORO-1368 the store is read from a background
/// queue while the main thread may be writing to it, and a test double with a
/// data race of its own would surface as a flake in the code under test.
final class InMemoryCredentialStore: CredentialStore, @unchecked Sendable {
    private let lock = NSLock()
    private var secrets: [String: String] = [:]
    private let unreadableKeys: Set<String>

    /// `unreadableKeys` makes the keychain's refusal reproducible without a
    /// keychain. It is the only way to cover the degraded-provider path
    /// (HORO-1368 AC 6) in a test: the real condition needs a binary whose code
    /// identity an existing item's ACL rejects, which cannot be arranged from
    /// inside the test bundle that would have to observe it.
    init(secrets: [String: String] = [:], unreadableKeys: Set<String> = []) {
        self.secrets = secrets
        self.unreadableKeys = unreadableKeys
    }

    func availability(forKey key: String) -> CredentialAvailability {
        if unreadableKeys.contains(key) { return .unreadable }
        return secret(forKey: key) == nil ? .absent : .present
    }

    func secret(forKey key: String) -> String? {
        if unreadableKeys.contains(key) { return nil }
        lock.lock()
        defer { lock.unlock() }
        guard let secret = secrets[key], !secret.isEmpty else { return nil }
        return secret
    }

    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        secrets[key] = secret
        return true
    }

    @discardableResult
    func deleteSecret(forKey key: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        secrets.removeValue(forKey: key)
        return true
    }
}
