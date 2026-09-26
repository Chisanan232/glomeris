//
//  CredentialAccessListTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1474 AC 6. The two states worth pinning: an empty trusted list, and an
//  entry whose binary is gone.
//
//  No keychain item is created, read or deleted here, and nothing on the disk is
//  removed. The classification under test is a pure function of an external
//  representation plus one existence predicate, and the predicate is injected —
//  which is the only way the dangling case is reachable at all. Producing it for
//  real needs an item created while a binary existed and read back after that
//  binary was deleted; on a build machine there is nothing safe to delete, and on
//  a developer machine the thing to delete would be an installed app.
//
//  One case *is* asserted against real Security-framework data rather than a
//  constructed fixture: `KeychainCredentialStore.selfOnlyAccess()` builds the
//  access specification `setSecret` actually attaches to the item, and its single
//  trusted application is this test binary. So the same access specification is
//  read twice below — once with the real existence check, where it resolves, and
//  once with an injected false, where it goes dangling. That is what makes the
//  dangling branch a fact about the shipped function rather than about a Data
//  literal written to match it.
//
//  Nothing here involves a credential. No value is stored or read.
//

import Security
import XCTest

final class CredentialAccessListTests: XCTestCase {

    // MARK: - The external representation, as measured

    /// A trusted application's external representation is a bare NUL-terminated
    /// POSIX path. Measured: `/bin/ls` comes back as 8 bytes with no signature,
    /// no CDHash and no alias record in it.
    func testAPathIsReadFromANulTerminatedRepresentation() {
        let representation = Data("/Applications/Glomeris.app\0".utf8)

        XCTAssertEqual(
            CredentialTrustedApplicationReading.path(fromExternalRepresentation: representation),
            "/Applications/Glomeris.app")
    }

    /// Trailing bytes after the terminator are ignored rather than rejected. A
    /// future macOS appending anything should still yield the path it starts
    /// with: an entry named but unverified is more use to the reader than an
    /// entry silently rendered as unidentifiable.
    func testBytesAfterTheTerminatorAreIgnored() {
        var representation = Data("/Applications/Glomeris.app\0".utf8)
        representation.append(contentsOf: [0x01, 0x02, 0x03])

        XCTAssertEqual(
            CredentialTrustedApplicationReading.path(fromExternalRepresentation: representation),
            "/Applications/Glomeris.app")
    }

    func testAnEmptyRepresentationNamesNothing() {
        XCTAssertNil(
            CredentialTrustedApplicationReading.path(fromExternalRepresentation: Data()))
        XCTAssertNil(
            CredentialTrustedApplicationReading.path(
                fromExternalRepresentation: Data([0x00, 0x00])))
    }

    /// A relative path is refused rather than displayed. There is no directory
    /// for it to be relative to on a settings screen, so it would be a string
    /// the user cannot act on presented as a location.
    func testARelativePathIsNotTreatedAsAPath() {
        XCTAssertNil(
            CredentialTrustedApplicationReading.path(
                fromExternalRepresentation: Data("Glomeris.app\0".utf8)))
    }

    // MARK: - AC 6, the dangling case

    /// A path with no code object at it is `dangling`, not dropped and not an
    /// error. This is the state a user reaches by replacing the app, and the
    /// whole point of showing the list is that they can see it.
    func testAnEntryWhoseBinaryIsGoneIsDangling() {
        let entry = CredentialTrustedApplicationReading.entry(
            index: 0,
            externalRepresentation: Data("/Applications/Glomeris 0.1.0.app\0".utf8),
            codeObjectExists: { _ in false })

        XCTAssertEqual(entry.reference, .dangling(path: "/Applications/Glomeris 0.1.0.app"))
        XCTAssertTrue(entry.isDangling)
        XCTAssertEqual(entry.path, "/Applications/Glomeris 0.1.0.app")
    }

    /// The other half of the same decision, so the assertion above is about the
    /// predicate rather than about the function always saying `dangling`.
    func testAnEntryWhoseBinaryIsPresentResolves() {
        let entry = CredentialTrustedApplicationReading.entry(
            index: 3,
            externalRepresentation: Data("/Applications/Glomeris.app\0".utf8),
            codeObjectExists: { _ in true })

        XCTAssertEqual(entry.reference, .resolves(path: "/Applications/Glomeris.app"))
        XCTAssertFalse(entry.isDangling)
        // The index is the ACL position, and it is the only thing distinguishing
        // two entries that name the same path.
        XCTAssertEqual(entry.id, 3)
    }

    /// The existence check is handed the path that was read, not the raw
    /// representation. A predicate receiving a string with a NUL in it would
    /// answer `false` for every entry, which would render every list as entirely
    /// stale — and would look exactly like a correct implementation on a machine
    /// where the app really had been replaced.
    func testTheExistenceCheckReceivesTheParsedPath() {
        var seen: [String] = []
        _ = CredentialTrustedApplicationReading.entry(
            index: 0,
            externalRepresentation: Data("/Applications/Glomeris.app\0".utf8),
            codeObjectExists: { path in
                seen.append(path)
                return true
            })

        XCTAssertEqual(seen, ["/Applications/Glomeris.app"])
    }

    /// An unreadable representation is counted, not omitted. An entry that
    /// cannot be named is still an entry, and a shorter list than the item has
    /// would understate who can read the key.
    func testAnUnnamedEntryIsKeptRatherThanDropped() {
        let entry = CredentialTrustedApplicationReading.entry(
            index: 0,
            externalRepresentation: Data(),
            codeObjectExists: { _ in
                XCTFail("an entry with no path must not be handed to the existence check")
                return true
            })

        XCTAssertEqual(entry.reference, .unnamed)
        XCTAssertNil(entry.path)
        XCTAssertFalse(entry.isDangling)
    }

    // MARK: - AC 6, the empty case

    /// An ACL with no decrypt entry yields an empty list, which is a real answer:
    /// no binary is pre-approved. The alternative — treating it as a failure —
    /// would report the strictest state there is as a fault.
    func testAnAclWithNothingToDecryptYieldsAnEmptyList() {
        XCTAssertEqual(
            CredentialTrustedApplicationReading.decryptApplications(inACLList: []),
            [])
    }

    /// And the in-memory store's default says the same thing for a stored key
    /// with no supplied list, so a test that does not care about the ACL does not
    /// accidentally assert a populated one.
    func testTheInMemoryStoreDefaultsToAnEmptyListForAStoredKey() {
        let store = InMemoryCredentialStore(secrets: ["k": "not-a-credential"])

        XCTAssertEqual(store.accessList(forKey: "k"), .applications([]))
        XCTAssertEqual(store.accessList(forKey: "absent"), .noItem)
    }

    // MARK: - The real access specification, read both ways

    /// The access specification `setSecret` attaches, read by the function the
    /// settings pane uses. One trusted application — this test binary — and it
    /// resolves, because it is running.
    ///
    /// This is the anti-vacuity assertion for the whole file: it proves
    /// `decryptApplications` can find an entry in a genuine `SecAccess` at all,
    /// so the empty-list test above is about an empty ACL rather than about a
    /// reader that never finds anything.
    func testTheShippedAccessSpecificationTrustsExactlyOneResolvingApplication() throws {
        let access = try XCTUnwrap(
            KeychainCredentialStore.selfOnlyAccess(),
            "could not build the self-only access specification in this process")
        var aclList: CFArray?
        XCTAssertEqual(SecAccessCopyACLList(access, &aclList), errSecSuccess)
        let acls = try XCTUnwrap(aclList as? [SecACL])

        let entries = CredentialTrustedApplicationReading.decryptApplications(inACLList: acls)

        XCTAssertEqual(entries.count, 1, "expected exactly one trusted application, per HORO-1455")
        let entry = try XCTUnwrap(entries.first)
        XCTAssertFalse(
            entry.isDangling,
            """
            The one application trusted by a freshly built access specification is \
            this running test binary, so its path must resolve. Reading it as \
            dangling means the existence check is answering false for a path that \
            exists, which on a real install would render every entry as stale.
            """)
        let path = try XCTUnwrap(entry.path)
        XCTAssertTrue(path.hasPrefix("/"), "the entry named \(path), which is not an absolute path")
    }

    /// The same real ACL, with the existence check inverted. The dangling branch
    /// is reached from Security-framework data rather than from a `Data` literal,
    /// and nothing was deleted to get there.
    func testTheSameRealAclGoesDanglingWhenNoCodeObjectExists() throws {
        let access = try XCTUnwrap(KeychainCredentialStore.selfOnlyAccess())
        var aclList: CFArray?
        XCTAssertEqual(SecAccessCopyACLList(access, &aclList), errSecSuccess)
        let acls = try XCTUnwrap(aclList as? [SecACL])

        let entries = CredentialTrustedApplicationReading.decryptApplications(
            inACLList: acls, codeObjectExists: { _ in false })

        XCTAssertEqual(entries.count, 1)
        XCTAssertTrue(
            entries.first?.isDangling == true,
            """
            An entry naming a path with no code object at it was not reported as \
            dangling. That is the defect HORO-1474 is about: the list would show \
            every stale approval as fine, because the call that returns the path \
            keeps succeeding after the binary it names is deleted.
            """)
    }

    /// `SecTrustedApplicationCopyData` succeeding is not evidence that anything
    /// is installed — measured, it keeps succeeding after the named binary is
    /// deleted — so the existence check has to be a separate step. Asserted by
    /// showing the real check disagrees with a path that cannot exist.
    func testTheExistenceCheckRejectsAPathThatIsNotThere() {
        let absent = "/Applications/GlomerisThatWasNeverInstalled-h1474.app"

        XCTAssertFalse(
            FileManager.default.fileExists(atPath: absent),
            "test precondition: the path must not exist")
        XCTAssertFalse(
            CredentialTrustedApplicationReading.codeObjectExists(atPath: absent),
            "a path with nothing at it was reported as holding a code object")
    }

    /// And accepts one that is. `/System/Applications/Calculator.app` ships with
    /// macOS and is signed, so this is the positive control for the assertion
    /// above — without it, a `codeObjectExists` that returned false
    /// unconditionally would pass.
    func testTheExistenceCheckAcceptsASignedSystemApplication() {
        let present = "/System/Applications/Calculator.app"

        XCTAssertTrue(
            FileManager.default.fileExists(atPath: present),
            "test precondition: the reference application must be installed")
        XCTAssertTrue(
            CredentialTrustedApplicationReading.codeObjectExists(atPath: present),
            "a signed, installed application was reported as having no code object")
    }
}

/// What the settings store does with each answer (HORO-1474 AC 4, AC 5).
///
/// The classification is covered above. This is the layer that maps a store
/// reading onto what the pane renders, and the one claim that has to be about
/// calls rather than values: reading the list must not read the key.
final class CredentialAccessResolutionTests: XCTestCase {
    private static let account = GlomerisLlmSettingsStore.apiKeyAccount
    private static let notACredential = "h1474-not-a-credential"

    private func makeDefaults() -> UserDefaults {
        let suiteName = "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        addTeardownBlock { defaults.removePersistentDomain(forName: suiteName) }
        return defaults
    }

    private func makeStore(credentials: CredentialStore) -> GlomerisLlmSettingsStore {
        GlomerisLlmSettingsStore(
            defaults: makeDefaults(),
            credentials: credentials,
            // Its own, never `.shared`: a test that let the shared record be
            // missed would make every later keychain test in this bundle resolve
            // as unresponsive without asking anything.
            keychainDeadline: KeychainDeadline(seconds: 5))
    }

    /// AC 4, and the assertion that makes it a test rather than a comment.
    /// Retrieving the value is what needs authorisation and what can raise a
    /// prompt; reading the ACL must not do it.
    func testReadingTheListDoesNotReadTheKey() async {
        let credentials = RecordingCredentialStore(
            secrets: [Self.account: Self.notACredential])
        let store = makeStore(credentials: credentials)

        _ = await store.resolvedAccessList()

        XCTAssertEqual(credentials.operations, [.accessList])
        XCTAssertFalse(
            credentials.operations.contains(.secret),
            """
            Reading who may read the key read the key. Retrieving the value is the \
            operation that needs the item decrypted, which is the operation that \
            can block behind an authorisation prompt — the whole reason this query \
            asks for a reference and not for data.
            """)
    }

    /// And it happens off the main thread, like every other keychain touch in
    /// this app (HORO-1368).
    func testTheListIsReadOffTheMainThread() async {
        let credentials = RecordingCredentialStore(
            secrets: [Self.account: Self.notACredential])
        let store = makeStore(credentials: credentials)

        _ = await store.resolvedAccessList()

        XCTAssertEqual(
            credentials.touches.filter(\.wasOnMainThread), [],
            "a keychain call on the main thread is how the menu-bar item disappears")
    }

    /// The three store-level readings pass through unchanged. `unresponsive` is
    /// the store's fourth case and cannot be produced here, which is the point:
    /// it is the deadline's answer, and it is covered in
    /// `GlomerisLlmSettingsKeychainDeadlineTests`.
    func testEachStoreReadingMapsToTheMatchingAppLevelCase() async {
        let entry = CredentialTrustedApplication(
            reference: .dangling(path: "/Applications/Glomeris 0.1.0.app"), index: 0)

        let cases: [(CredentialAccessListReading, GlomerisCredentialAccess)] = [
            (.applications([entry]), .applications([entry])),
            (.applications([]), .applications([])),
            (.noItem, .noItem),
            (.unreadable, .unreadable),
        ]

        for (reading, expected) in cases {
            let store = makeStore(
                credentials: InMemoryCredentialStore(
                    secrets: [Self.account: Self.notACredential],
                    accessLists: [Self.account: reading]))

            let resolved = await store.resolvedAccessList()

            XCTAssertEqual(resolved, expected, "\(reading) did not map to \(expected)")
        }
    }

    /// A store with no item answers `noItem` rather than an empty list. The two
    /// render very differently — one has no list to show, the other is a list
    /// stating that nothing is pre-approved.
    func testNoStoredKeyIsNotAnEmptyList() async {
        let store = makeStore(credentials: InMemoryCredentialStore())

        let resolved = await store.resolvedAccessList()

        XCTAssertEqual(resolved, .noItem)
        XCTAssertNotEqual(resolved, .applications([]))
    }
}

/// The sentences (HORO-1474 AC 1, AC 2, AC 3).
///
/// Pure functions, asserted directly rather than through the view, for the same
/// reason `ProjectRootsWording` is: the wording is the deliverable, and a test
/// that rendered a `View` to reach it would assert on layout instead.
final class CredentialAccessWordingTests: XCTestCase {
    private func resolving(_ path: String, at index: Int = 0) -> CredentialTrustedApplication {
        CredentialTrustedApplication(reference: .resolves(path: path), index: index)
    }

    private func dangling(_ path: String, at index: Int = 0) -> CredentialTrustedApplication {
        CredentialTrustedApplication(reference: .dangling(path: path), index: index)
    }

    // MARK: - AC 1: the summary

    /// An empty list is the strictest state there is, and has to read that way.
    /// Worded as an absence a user could reasonably try to fix it — by pasting a
    /// key, which is the one action that would replace the item.
    func testAnEmptyListReadsAsAProtectionAndNotAsSomethingMissing() {
        let summary = GlomerisCredentialAccessWording.summary(.applications([]))

        XCTAssertTrue(
            summary.contains("No app is pre-approved"),
            "an empty list must say what it means: nothing may read the key unasked")
        XCTAssertTrue(
            summary.contains("strictest"),
            "without this the sentence reads as a fault rather than as protection")
        XCTAssertFalse(
            summary.lowercased().contains("paste"),
            "there is nothing to fix here, so there must be no instruction to act")
    }

    func testTheSummaryCountsTheApplications() {
        XCTAssertTrue(
            GlomerisCredentialAccessWording
                .summary(.applications([resolving("/Applications/Glomeris.app")]))
                .contains("1 app may read this key"))
        XCTAssertTrue(
            GlomerisCredentialAccessWording
                .summary(
                    .applications([
                        resolving("/Applications/Glomeris.app"),
                        resolving("/Applications/Old.app", at: 1),
                    ])
                )
                .contains("2 apps may read this key"))
    }

    // MARK: - AC 2: stale entries are named as such

    /// The state the ticket was filed about: entries left behind by replaced
    /// builds. Counted, explained, and explicitly not called an error.
    func testStaleEntriesAreCountedAndExplained() {
        let summary = GlomerisCredentialAccessWording.summary(
            .applications([
                resolving("/Applications/Glomeris.app"),
                dangling("/Applications/Glomeris 0.1.0.app", at: 1),
                dangling("/Applications/Glomeris 0.2.0.app", at: 2),
            ]))

        XCTAssertTrue(summary.contains("3 apps may read this key"))
        XCTAssertTrue(
            summary.contains("2 of them are no longer on this Mac"),
            "the count of stale entries is the number the user is deciding about")
        XCTAssertTrue(
            summary.contains("replacing the app after the key was saved"),
            "an unexplained stale entry reads as a break-in rather than as an upgrade")
    }

    func testASingleStaleEntryIsCountedInTheSingular() {
        let summary = GlomerisCredentialAccessWording.summary(
            .applications([dangling("/Applications/Glomeris 0.1.0.app")]))

        XCTAssertTrue(summary.contains("1 app may read this key"))
        XCTAssertTrue(summary.contains("1 of them is no longer on this Mac"))
    }

    /// A row says the path holds no app, and stops there. What macOS would do
    /// with a *different* app installed at the same path was not measured — the
    /// stored entry carries a path and no signature, so it is a live question —
    /// and a screen that guessed at it would be inventing a security property.
    func testAStaleRowClaimsOnlyWhatWasMeasured() {
        let note = GlomerisCredentialAccessWording.rowNote(
            dangling("/Applications/Glomeris 0.1.0.app"))

        XCTAssertEqual(note, "No app is installed at this path now.")
        XCTAssertNil(
            GlomerisCredentialAccessWording.rowNote(resolving("/Applications/Glomeris.app")),
            "a live entry has nothing to qualify")
    }

    /// The path is shown whole. Two entries can differ only in their directory,
    /// so a bundle name would make them indistinguishable.
    func testARowShowsThePathVerbatim() {
        XCTAssertEqual(
            GlomerisCredentialAccessWording.rowTitle(resolving("/Applications/Glomeris.app")),
            "/Applications/Glomeris.app")
        XCTAssertEqual(
            GlomerisCredentialAccessWording.rowTitle(
                CredentialTrustedApplication(reference: .unnamed, index: 0)),
            "An app this screen could not identify")
    }

    /// HORO-1451's composer, so a row is not heard as one run-on clause.
    func testARowsSpokenLabelIsComposedAndTerminated() {
        let spoken = GlomerisCredentialAccessWording.spokenLabel(
            dangling("/Applications/Glomeris 0.1.0.app"))

        XCTAssertEqual(
            spoken,
            "Approved app: /Applications/Glomeris 0.1.0.app. No app is installed at this path now."
        )
        // The rule itself, not a restatement of it: the composer's own output for
        // the same clauses.
        XCTAssertEqual(
            spoken,
            SpokenLabel.compose([
                "Approved app: /Applications/Glomeris 0.1.0.app",
                "No app is installed at this path now.",
            ]))
    }

    func testALiveRowsSpokenLabelIsStillTerminated() {
        let spoken = GlomerisCredentialAccessWording.spokenLabel(
            resolving("/Applications/Glomeris.app"))

        XCTAssertEqual(spoken, "Approved app: /Applications/Glomeris.app.")
    }

    // MARK: - AC 3: why there is no reset button

    /// The statement branch of AC 3. It has to contain three things, and each one
    /// is a separate failure if it goes missing.
    func testTheResetExplanationSaysWhatTheOnlyResetIsAndWhatItCosts() throws {
        let explanation = try XCTUnwrap(
            GlomerisCredentialAccessWording.resetExplanation([
                resolving("/Applications/Glomeris.app"),
                dangling("/Applications/Glomeris 0.1.0.app", at: 1),
            ]))

        XCTAssertTrue(
            explanation.contains("no way to shorten this list in place"),
            "a user who is not told this will go looking for the button")
        XCTAssertTrue(
            explanation.contains("press Save"),
            "the reset that does exist has to be named, or the sentence is only bad news")
        XCTAssertTrue(
            explanation.contains("cannot read a stored key back"),
            """
            The cost has to be stated before the user acts on it. Saving replaces \
            the item, and this app has no getter for a stored key, so a user who \
            no longer has their key would be left with none.
            """)
        XCTAssertTrue(
            explanation.contains("Keychain Access"),
            "the route that does not need this app has to be there too (HORO-1455 AC 8)")
    }

    /// And it is absent when there is nothing to shorten. An empty list is
    /// already the strictest state; a single live entry is the state a reset
    /// produces. Offering the instructions there would invite a user to re-paste
    /// a key to reach the state they are in.
    func testTheResetExplanationIsAbsentWhenThereIsNothingToShorten() {
        XCTAssertNil(GlomerisCredentialAccessWording.resetExplanation([]))
        XCTAssertNil(
            GlomerisCredentialAccessWording.resetExplanation([
                resolving("/Applications/Glomeris.app")
            ]))
    }

    /// One stale entry is worth resetting even though the list is length one —
    /// the approval outlived the build it named, and that is the whole subject of
    /// the ticket.
    func testASingleStaleEntryIsStillWorthResetting() {
        XCTAssertNotNil(
            GlomerisCredentialAccessWording.resetExplanation([
                dangling("/Applications/Glomeris 0.1.0.app")
            ]))
    }

    // MARK: - The degraded answers

    /// Silence and refusal get different sentences, and neither is worded as an
    /// empty list.
    func testSilenceAndRefusalAreWordedApart() {
        let unresponsive = GlomerisCredentialAccessWording.summary(.unresponsive)
        let unreadable = GlomerisCredentialAccessWording.summary(.unreadable)

        XCTAssertNotEqual(unresponsive, unreadable)
        XCTAssertTrue(unresponsive.contains("did not answer"))
        XCTAssertTrue(
            unresponsive.contains("Nothing has been changed"),
            "the first fear on seeing this is that the key was lost")
        XCTAssertTrue(unreadable.contains("would not say"))
        for summary in [unresponsive, unreadable] {
            XCTAssertFalse(
                summary.contains("No app is pre-approved"),
                "a degraded answer must not be reported as the strictest state")
        }
    }

    func testNoStoredKeyIsWordedAsHavingNoList() {
        XCTAssertTrue(
            GlomerisCredentialAccessWording.summary(.noItem).contains("no list"))
    }
}
