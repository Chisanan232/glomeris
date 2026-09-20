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
protocol CredentialStore {
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
    /// Matches the UserDefaults suite name and the bundle identifier, so all
    /// of this app's stored state is findable under one name.
    static let service = "dev.glomeris.GlomerisMenuBar"

    private let service: String

    init(service: String = KeychainCredentialStore.service) {
        self.service = service
    }

    private func baseQuery(forKey key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
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

    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool {
        guard let data = secret.data(using: .utf8) else { return false }

        // Update-then-add rather than add-then-handle-duplicate: an existing
        // item must be replaced in place so its access control and creation
        // date survive, and so a failed add can never leave the old key
        // behind while the UI says the new one was saved.
        let updated = SecItemUpdate(
            baseQuery(forKey: key) as CFDictionary,
            [kSecValueData as String: data] as CFDictionary
        )
        if updated == errSecSuccess { return true }

        var insert = baseQuery(forKey: key)
        insert[kSecValueData as String] = data
        // Available whenever the user has unlocked the Mac, and never synced
        // to iCloud or included in a backup: a BYOK key is local to the
        // machine the CLI runs on, so ThisDeviceOnly is both the tighter and
        // the more accurate choice.
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        return SecItemAdd(insert as CFDictionary, nil) == errSecSuccess
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
final class InMemoryCredentialStore: CredentialStore {
    private var secrets: [String: String] = [:]

    init(secrets: [String: String] = [:]) {
        self.secrets = secrets
    }

    func secret(forKey key: String) -> String? {
        guard let secret = secrets[key], !secret.isEmpty else { return nil }
        return secret
    }

    @discardableResult
    func setSecret(_ secret: String, forKey key: String) -> Bool {
        secrets[key] = secret
        return true
    }

    @discardableResult
    func deleteSecret(forKey key: String) -> Bool {
        secrets.removeValue(forKey: key)
        return true
    }
}
