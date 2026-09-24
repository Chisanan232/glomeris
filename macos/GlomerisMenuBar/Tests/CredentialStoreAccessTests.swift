//
//  CredentialStoreAccessTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1455 AC 4. What the BYOK key's keychain item is created with.
//
//  These tests never touch a keychain. They assert on the dictionary
//  `KeychainCredentialStore.insertAttributes(forKey:data:)` builds, which is the
//  exact dictionary `setSecret` hands to `SecItemAdd` and nothing more.
//
//  That indirection is deliberate rather than convenient. `SecItemAdd` cannot
//  run here: an unsigned `xcodebuild` test binary writing to the login keychain
//  gets `errSecAuthFailed` (-25293), and one asking for the data-protection
//  keychain gets `errSecMissingEntitlement` (-34018) — both measured. A test
//  that called it and skipped itself on those statuses would be the HORO-1253
//  failure mode: a gate that never ran, reading as a gate that passed. So the
//  attributes are asserted where no entitlement is needed and nothing can be
//  skipped, and the guard script asserts `setSecret` really does pass them on.
//
//  Nothing here involves a credential. The bytes written below are a fixed
//  non-secret literal, no test reads a stored value, and the real item's service
//  is never queried.
//

import Security
import XCTest

final class CredentialStoreAccessTests: XCTestCase {
    /// Deliberately unmistakable for a real key.
    private let notASecret = Data("h1455-test-not-a-credential".utf8)
    private let account = "h1455-test-account"

    private func attributes() -> [String: Any] {
        KeychainCredentialStore(service: "dev.glomeris.h1455-test")
            .insertAttributes(forKey: account, data: notASecret)
    }

    // MARK: - AC 2, the declared trusted list

    /// The test AC 4 is actually about: removing `kSecAttrAccess` from
    /// `insertAttributes` must fail something. Before this ticket the key was
    /// absent and the item inherited a default ACL, so nothing in the suite
    /// could tell the two apart.
    func testCreatedItemCarriesAnExplicitAccessSpecification() {
        let insert = attributes()

        XCTAssertNotNil(
            insert[kSecAttrAccess as String],
            """
            The created item carries no kSecAttrAccess, so its ACL is whatever \
            the keychain defaults to. That default happens to be self-only \
            today — which is why this has to be asserted rather than observed: \
            the two are indistinguishable from the outside, and only the \
            declared one stays correct if the default changes.
            """)
    }

    /// The assertion that makes the one above worth having. An explicit
    /// `kSecAttrAccess` whose list has grown is worse than none, because it
    /// reads as deliberate.
    func testTheTrustedListContainsExactlyOneApplication() throws {
        let insert = attributes()
        let access = try XCTUnwrap(
            insert[kSecAttrAccess as String] as! SecAccess?,
            "no access specification to inspect")

        var aclList: CFArray?
        // SecAccess/SecACL are deprecated with the rest of SecKeychain. A file
        // keychain item has no other ACL, so this is the only way to read the
        // one under test. See KeychainCredentialStore.selfOnlyAccess().
        XCTAssertEqual(
            SecAccessCopyACLList(access, &aclList), errSecSuccess,
            "the access specification has no readable ACL list")
        let acls = try XCTUnwrap(aclList as? [SecACL], "ACL list was not an array of SecACL")

        // Only the decrypt entry decides who may read the value, which is the
        // accretion HORO-1455 is about. The encrypt entry trusts everything by
        // design (anyone may write a new item) and change_acl trusts nothing.
        var decryptEntries = 0
        for acl in acls {
            let authorizations = (SecACLCopyAuthorizations(acl) as? [String]) ?? []
            guard authorizations.contains(kSecACLAuthorizationDecrypt as String) else { continue }

            var applications: CFArray?
            var description: CFString?
            var prompt = SecKeychainPromptSelector()
            XCTAssertEqual(
                SecACLCopyContents(acl, &applications, &description, &prompt), errSecSuccess,
                "the decrypt entry's contents were unreadable")

            let trusted = applications as? [SecTrustedApplication]
            XCTAssertEqual(
                trusted?.count, 1,
                """
                The decrypt entry trusts \(trusted?.count.description ?? "every application") \
                rather than exactly one. A list longer than one means some other \
                binary can read the user's provider key; a nil list means every \
                binary can. Either way this is the widening HORO-1455 exists to \
                stop, and it is now in the source rather than accreted at runtime.
                """)
            decryptEntries += 1
        }

        // An ACL with no decrypt entry would make the loop above vacuous, and a
        // vacuous loop is how this test would silently stop checking anything.
        XCTAssertEqual(
            decryptEntries, 1,
            "expected exactly one decrypt entry to inspect, found \(decryptEntries)")
    }

    /// `selfOnlyAccess()` returning nil is tolerated in production — the item is
    /// created with the default ACL rather than not at all. Here it would make
    /// every assertion above pass by never running, so it is asserted directly.
    func testSelfOnlyAccessIsAvailableInThisProcess() {
        XCTAssertNotNil(
            KeychainCredentialStore.selfOnlyAccess(),
            """
            Could not build a self-only access specification in the test \
            process. The tests above would then be asserting about a nil that \
            production tolerates, so they would pass while proving nothing.
            """)
    }

    // MARK: - AC 1, the attribute that is set but inert

    func testCreatedItemIsMarkedThisDeviceOnly() {
        let insert = attributes()

        XCTAssertEqual(
            insert[kSecAttrAccessible as String] as! CFString?,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            """
            The accessibility attribute is inert on a file keychain item, but it \
            is the value to already be carrying if this app is ever signed and \
            entitled. Changing it to a syncable one would be a decision, not a \
            tidy-up.
            """)
    }

    /// Pins the founder decision recorded on HORO-1455 rather than pre-empting
    /// it. Moving to the data-protection keychain is a real option with three
    /// measured costs; this only makes the move visible instead of incidental.
    func testCreatedItemDoesNotSilentlyMoveToTheDataProtectionKeychain() {
        let insert = attributes()

        XCTAssertNil(
            insert[kSecUseDataProtectionKeychain as String],
            """
            The item now targets the data-protection keychain. That is a \
            founder decision on HORO-1455 with costs attached — it needs an \
            entitlement this app does not have (measured: -34018), the item \
            stops being visible and revocable in Keychain Access, and existing \
            stored keys do not migrate. If the decision was taken, update this \
            test and book/src/byok.md with it.
            """)
    }

    // MARK: - The rest of the dictionary

    func testCreatedItemIsAGenericPasswordUnderTheStoreService() {
        let insert = attributes()

        XCTAssertEqual(insert[kSecClass as String] as! CFString?, kSecClassGenericPassword)
        XCTAssertEqual(insert[kSecAttrService as String] as? String, "dev.glomeris.h1455-test")
        XCTAssertEqual(insert[kSecAttrAccount as String] as? String, account)
        XCTAssertEqual(
            insert[kSecValueData as String] as? Data, notASecret,
            "the value handed to SecItemAdd is not the value passed in")
    }

    /// The production service is derived, not literal — HORO-1456. Asserted here
    /// too because `insertAttributes` is where it reaches the keychain.
    func testTheDefaultServiceIsTheRunningBundleIdentifier() {
        let insert = KeychainCredentialStore().insertAttributes(forKey: account, data: notASecret)

        XCTAssertEqual(insert[kSecAttrService as String] as? String, BundleIdentity.current)
    }
}
