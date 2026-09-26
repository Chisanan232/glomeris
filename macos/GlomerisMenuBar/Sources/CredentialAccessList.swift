//
//  CredentialAccessList.swift
//  GlomerisMenuBar
//
//  HORO-1474: which binaries may read the stored provider key, read from the
//  keychain item's access control list and never from its value.
//
//  See the standing project rule in GlomerisMenuBarApp.swift. This file makes
//  no policy decision and takes no action: it reads an ACL, names what it
//  found, and says whether each named path still has a code object at it.
//
//  ---------------------------------------------------------------------
//  WHY THIS EXISTS
//  ---------------------------------------------------------------------
//  HORO-1455 stopped the item's trusted-application list from growing: saving
//  a key now deletes and re-adds the item, so the list is rebuilt from one
//  entry instead of inheriting every "Always Allow" the user ever answered for
//  a build that has since been replaced. What it could not do is show the user
//  the list. The only place it surfaces in macOS is the authorisation prompt,
//  and a prompt names the one binary asking — never the accumulated list. On
//  the workstation HORO-1455 was measured on, that list had reached 23 entries
//  before anyone looked at it.
//
//  ---------------------------------------------------------------------
//  WHAT A TRUSTED APPLICATION ACTUALLY IS, MEASURED
//  ---------------------------------------------------------------------
//  Every claim below was executed against purpose-built scratch keychains
//  under /private/tmp, never against the login keychain:
//
//    * `SecTrustedApplicationCopyData` returns a bare NUL-terminated POSIX
//      path and nothing else. 8 bytes for `/bin/ls`. There is no signature,
//      no CDHash and no alias record in it.
//    * The path is canonicalised up to the bundle. Asking for
//      `…/Calculator.app/Contents/MacOS/Calculator` stores
//      `/System/Applications/Calculator.app`.
//    * It succeeds unchanged after the binary it names is deleted. So it is
//      NOT the resolution check — a reading that only called this would report
//      every stale entry as fine.
//
//  That last point is why `codeObjectExists(atPath:)` exists, and why the
//  check it performs is a separate step rather than a status code from the
//  Security framework. For a path whose binary has been removed, measured:
//  `SecTrustedApplicationCreateFromPath` returns 100002 (POSIX ENOENT) and
//  `SecStaticCodeCreateWithPath` returns -67068 (errSecCSStaticCodeNotFound).
//
//  A correction to the status recorded on the ticket: -2147415734 is
//  CSSMERR_CSP_VERIFY_FAILED, a signature-verification failure, and it does
//  not reproduce for a bundle that has merely been deleted. It cannot: the
//  stored representation carries no signature for anything to verify against.
//
//  ---------------------------------------------------------------------
//  THE VALUE IS NEVER READ
//  ---------------------------------------------------------------------
//  AC 4, and it is a property of the query rather than of a convention. The
//  lookup below asks for `kSecReturnRef` and deliberately not
//  `kSecReturnData`: with no data requested the item is never decrypted, so
//  its ACL is not consulted, so no `SecurityAgent` prompt can appear — the
//  same reason `availability(forKey:)` asks only for attributes.
//  `scripts/check-credential-store-uses-keychain.sh` asserts that
//  `kSecReturnData` appears exactly once in the whole credential path, which
//  is the one place that genuinely needs it.
//

import Foundation
import Security

/// One entry in the stored key's trusted-application list.
struct CredentialTrustedApplication: Equatable, Identifiable {
    /// What the entry names, and whether it is still there.
    enum Reference: Equatable {
        /// Names a path where a code object still exists.
        case resolves(path: String)

        /// Names a path where none does.
        ///
        /// The normal state after a few upgrades, not an error — which is what
        /// AC 2 turns on. A list of stale references is exactly what the user
        /// needs to see, so this is a first-class case rather than something
        /// filtered out or rendered as a failure.
        case dangling(path: String)

        /// The entry carries no path this app could read.
        ///
        /// Kept rather than dropped for the same reason as `dangling`: an entry
        /// that cannot be named is still an entry, and a list that silently
        /// omitted it would understate who can read the key. Not reachable from
        /// anything measured — every representation examined was a plain path —
        /// so it exists to avoid the one outcome that would be wrong, which is
        /// a shorter list than the item really has.
        case unnamed
    }

    let reference: Reference

    /// Position in the ACL's application list, which is the only thing that
    /// tells two entries naming the same path apart. `Identifiable` needs
    /// something stable, and a path is not unique.
    let index: Int

    var id: Int { index }

    /// The path named, or `nil` for `unnamed`.
    var path: String? {
        switch reference {
        case .resolves(let path), .dangling(let path): return path
        case .unnamed: return nil
        }
    }

    var isDangling: Bool {
        if case .dangling = reference { return true }
        return false
    }
}

/// The stored item's trusted-application list, or why there is none to show.
///
/// The store-level type. It has no "did not answer in time" case because a
/// store does not own a deadline — see `GlomerisCredentialAccess`, which does,
/// exactly as `GlomerisLlmSettingSource` does for `CredentialAvailability`.
enum CredentialAccessListReading: Equatable {
    /// The applications trusted to decrypt the item, in ACL order.
    ///
    /// An empty array is a real answer and not a failure: it means no binary is
    /// pre-trusted, so every read has to be authorised by the user. That is the
    /// most restrictive state an item can be in, and wording it as "none" would
    /// invert its meaning.
    case applications([CredentialTrustedApplication])

    /// No item is stored for this key, so there is no ACL to read.
    case noItem

    /// An item may exist, but the keychain would not say what its ACL is.
    case unreadable
}

/// Turning one trusted application, and one ACL, into something displayable.
///
/// Free functions over data rather than methods on the store, so the two cases
/// AC 6 pins — an empty list, and an entry whose binary is gone — are asserted
/// against the real classification without a keychain, without an entitlement
/// and without deleting anything on the machine running the test.
enum CredentialTrustedApplicationReading {
    /// The path a trusted application's external representation names.
    ///
    /// The representation is a NUL-terminated C string (measured — see the file
    /// comment). Everything from the first NUL on is ignored rather than
    /// rejected: a longer representation from some future macOS should still
    /// yield the path it starts with, because a named-but-unverified entry is
    /// more use to the reader than no entry at all.
    ///
    /// `nil` when there is no plausible path in it — empty, not valid UTF-8, or
    /// not absolute. A relative path would be meaningless to display, since
    /// there is no directory for it to be relative to.
    static func path(fromExternalRepresentation data: Data) -> String? {
        let cString = data.prefix { $0 != 0 }
        guard !cString.isEmpty,
            let text = String(data: Data(cString), encoding: .utf8),
            text.hasPrefix("/")
        else {
            return nil
        }
        return text
    }

    /// Whether a code object still lives at `path`.
    ///
    /// `SecStaticCodeCreateWithPath` and not `FileManager.fileExists`, because
    /// the two answer different questions and only one of them is the one worth
    /// putting on screen. An empty directory left behind at an uninstalled
    /// app's path exists; there is nothing there that could ever be trusted
    /// again. This does not *verify* the code — `SecStaticCodeCheckValidity`
    /// would, and a revoked or re-signed binary is a separate question this
    /// ticket does not ask.
    static func codeObjectExists(atPath path: String) -> Bool {
        var code: SecStaticCode?
        let status = SecStaticCodeCreateWithPath(
            URL(fileURLWithPath: path) as CFURL, [], &code)
        return status == errSecSuccess && code != nil
    }

    /// One entry, classified.
    ///
    /// `codeObjectExists` is injected so the dangling branch can be driven
    /// directly. A test that had to delete a binary to reach it would be a test
    /// that mutated the machine it ran on, and on a build machine it would have
    /// nothing safe to delete.
    static func entry(
        index: Int,
        externalRepresentation data: Data,
        codeObjectExists: (String) -> Bool = Self.codeObjectExists(atPath:)
    ) -> CredentialTrustedApplication {
        guard let path = path(fromExternalRepresentation: data) else {
            return CredentialTrustedApplication(reference: .unnamed, index: index)
        }
        return CredentialTrustedApplication(
            reference: codeObjectExists(path) ? .resolves(path: path) : .dangling(path: path),
            index: index
        )
    }

    /// Every application trusted to decrypt, across every ACL entry that
    /// authorises decryption.
    ///
    /// Across, not from the first: an item is free to carry more than one
    /// decrypt entry, and reading only one would understate the list. Entries
    /// authorising something else are skipped — the encrypt entry trusts
    /// everything by design (anyone may write a new item) and change-ACL trusts
    /// nothing, so neither says anything about who can read the key.
    ///
    /// An item with no decrypt entry at all yields an empty array, which is the
    /// same answer as a decrypt entry with an empty application list, and means
    /// the same thing: no binary is pre-trusted.
    static func decryptApplications(
        inACLList aclList: [SecACL],
        codeObjectExists: (String) -> Bool = Self.codeObjectExists(atPath:)
    ) -> [CredentialTrustedApplication] {
        var entries: [CredentialTrustedApplication] = []
        for acl in aclList {
            let authorizations = SecACLCopyAuthorizations(acl) as? [String] ?? []
            guard authorizations.contains(kSecACLAuthorizationDecrypt as String) else { continue }

            var applications: CFArray?
            var description: CFString?
            var promptSelector = SecKeychainPromptSelector()
            guard
                SecACLCopyContents(acl, &applications, &description, &promptSelector)
                    == errSecSuccess
            else {
                continue
            }

            for application in (applications as? [SecTrustedApplication]) ?? [] {
                var data: CFData?
                let status = SecTrustedApplicationCopyData(application, &data)
                // A representation this app cannot obtain still describes an
                // application that can read the key, so it is counted as an
                // unnamed entry rather than dropped.
                let representation = status == errSecSuccess ? (data as Data? ?? Data()) : Data()
                entries.append(
                    entry(
                        index: entries.count,
                        externalRepresentation: representation,
                        codeObjectExists: codeObjectExists))
            }
        }
        return entries
    }
}

extension KeychainCredentialStore {
    /// The stored item's trusted-application list.
    ///
    /// Synchronous and unbounded, exactly like `availability(forKey:)` and for
    /// the same measured reason: these are requests to `securityd`, and on a Mac
    /// where that daemon accepts the connection and never replies they do not
    /// return. Callers must impose a deadline — see
    /// `GlomerisLlmSettingsStore.resolvedAccessList()`, which is the one the
    /// settings screen uses (AC 5).
    ///
    /// `SecKeychain*` is deprecated wholesale. A file keychain item has no other
    /// ACL API, so there is no non-deprecated way to read this at all, which is
    /// the second argument on the founder decision open on HORO-1455: a
    /// data-protection keychain item has no trusted-application list, so moving
    /// to one would make this whole file unnecessary rather than modernise it.
    func accessList(forKey key: String) -> CredentialAccessListReading {
        var query = baseQuery(forKey: key)
        // Ref only. No `kSecReturnData`, so the item is never decrypted and no
        // authorisation prompt can appear (AC 4).
        query[kSecReturnRef as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var found: CFTypeRef?
        switch SecItemCopyMatching(query as CFDictionary, &found) {
        case errSecSuccess: break
        case errSecItemNotFound: return .noItem
        default: return .unreadable
        }

        // A data-protection keychain item comes back as a dictionary rather than
        // a `SecKeychainItem`, and force-casting one would be a crash in the
        // settings pane. Checked rather than assumed, even though this store
        // asks for neither.
        guard let found, CFGetTypeID(found) == SecKeychainItemGetTypeID() else {
            return .unreadable
        }
        // The type ID above is the real check. A *conditional* downcast to a
        // CoreFoundation type is rejected by the compiler ("will always
        // succeed"), which is exactly why it cannot be used as the guard.
        let item = found as! SecKeychainItem

        var access: SecAccess?
        guard SecKeychainItemCopyAccess(item, &access) == errSecSuccess, let access else {
            return .unreadable
        }

        var aclList: CFArray?
        guard SecAccessCopyACLList(access, &aclList) == errSecSuccess,
            let acls = aclList as? [SecACL]
        else {
            return .unreadable
        }

        return .applications(CredentialTrustedApplicationReading.decryptApplications(inACLList: acls))
    }
}
