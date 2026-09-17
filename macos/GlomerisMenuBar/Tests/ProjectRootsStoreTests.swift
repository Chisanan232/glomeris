//
//  ProjectRootsStoreTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1067: ProjectRootsStore persistence and argument-shaping tests.
//

import XCTest

// The test target compiles ProjectRootsStore.swift directly as one of its
// own sources (see project.yml) rather than importing the app module,
// matching GlomerisClientTests's pattern — GlomerisMenuBar is an
// `LSUIElement` app target with no importable framework product.

final class ProjectRootsStoreTests: XCTestCase {
    /// A fresh, uniquely-named `UserDefaults` suite per test, so tests
    /// never see each other's persisted state and never touch the real
    /// `dev.glomeris.GlomerisMenuBar` suite used by the app.
    private func makeDefaults() -> UserDefaults {
        let suiteName = "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        addTeardownBlock {
            defaults.removePersistentDomain(forName: suiteName)
        }
        return defaults
    }

    func testAddedRootPersistsAcrossFreshStoreInstance() {
        let defaults = makeDefaults()
        ProjectRootsStore(defaults: defaults).addRoot("/Users/dev/project-a")

        // A fresh store instance over the same UserDefaults simulates an
        // app restart — reading must not depend on any in-memory cache.
        let reopened = ProjectRootsStore(defaults: defaults)
        XCTAssertEqual(reopened.roots, ["/Users/dev/project-a"])
    }

    func testRemovingRootActuallyRemovesIt() {
        let defaults = makeDefaults()
        let store = ProjectRootsStore(defaults: defaults)
        store.addRoot("/Users/dev/project-a")
        store.addRoot("/Users/dev/project-b")

        store.removeRoot("/Users/dev/project-a")

        XCTAssertEqual(store.roots, ["/Users/dev/project-b"])
        // Removal takes effect on the next read with no restart needed —
        // confirm via a fresh instance too, same as the persistence test.
        XCTAssertEqual(ProjectRootsStore(defaults: defaults).roots, ["/Users/dev/project-b"])
    }

    func testDuplicateRootIsNotStoredTwice() {
        let defaults = makeDefaults()
        let store = ProjectRootsStore(defaults: defaults)

        store.addRoot("/Users/dev/project-a")
        store.addRoot("/Users/dev/project-a")

        XCTAssertEqual(store.roots, ["/Users/dev/project-a"])
    }

    func testCommandLineArgumentsShapeForMultipleRoots() {
        let defaults = makeDefaults()
        let store = ProjectRootsStore(defaults: defaults)
        store.addRoot("/Users/dev/project-a")
        store.addRoot("/Users/dev/project-b")

        XCTAssertEqual(
            store.commandLineArguments,
            ["--project-root", "/Users/dev/project-a", "--project-root", "/Users/dev/project-b"]
        )
    }

    func testCommandLineArgumentsEmptyWhenNoRootsConfigured() {
        let store = ProjectRootsStore(defaults: makeDefaults())

        XCTAssertTrue(store.commandLineArguments.isEmpty)
    }
}
