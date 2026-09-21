//
//  ProjectRootsPreferencesViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1325. This pane had no test file at all, which is part of why its
//  controls could ship nameless: there was nowhere for the claim "a VoiceOver
//  user can tell these buttons apart" to be written down.
//
//  Only the wording is asserted. Whether SwiftUI attaches a modifier is
//  SwiftUI's business; whether the string it attaches says something useful is
//  this project's, and it is the half that was wrong.
//

import XCTest

// The test target compiles ProjectRootsPreferencesView.swift directly as one
// of its own sources (see project.yml), matching ProjectRootsStoreTests and
// every other test here — GlomerisMenuBar is an `LSUIElement` app target with
// no importable framework product.

final class ProjectRootsPreferencesViewTests: XCTestCase {

    // MARK: - The remove control

    /// The defect, stated as a test: every row's button said the same nothing.
    func testRemoveLabelDistinguishesOneRowFromAnother() {
        let first = ProjectRootsWording.removeLabel(for: "/Users/dev/alpha")
        let second = ProjectRootsWording.removeLabel(for: "/Users/dev/beta")

        XCTAssertNotEqual(
            first,
            second,
            "two rows read identically, which is the whole defect this closes"
        )
        XCTAssertTrue(first.contains("/Users/dev/alpha"))
        XCTAssertFalse(first.contains("/Users/dev/beta"))
    }

    /// A name that says which path but not what will happen to it is only half
    /// an answer, and the dangerous half is the verb.
    func testRemoveLabelNamesTheAction() {
        let label = ProjectRootsWording.removeLabel(for: "/Users/dev/proj")

        XCTAssertTrue(label.lowercased().contains("remove"))
        XCTAssertFalse(
            label.lowercased().contains("minus"),
            "the glyph's name is not the action's name"
        )
    }

    /// Paths are what this list contains, so the label has to survive the
    /// awkward ones rather than assuming a tidy `/Users/x/y`.
    func testRemoveLabelPassesThePathThroughVerbatim() {
        for path in [
            "/Users/dev/My Project (old)",
            "/Users/dev/проект",
            "/",
            "relative/not/absolute",
        ] {
            XCTAssertTrue(
                ProjectRootsWording.removeLabel(for: path).hasSuffix(path),
                "\(path) was altered on its way into the label"
            )
        }
    }

    /// An empty root should never be in the store, but a label that silently
    /// becomes "Remove project root " if one ever is would hide that rather
    /// than expose it. Asserted so the behaviour is a decision, not an
    /// accident.
    func testRemoveLabelOfAnEmptyPathStillNamesTheAction() {
        XCTAssertTrue(ProjectRootsWording.removeLabel(for: "").lowercased().contains("remove"))
    }

    // MARK: - The field and the add control

    /// The field's placeholder is an example of what to type. It is not a name,
    /// and it is gone as soon as anything is typed — so the two must differ.
    func testFieldLabelIsNotThePlaceholder() {
        XCTAssertFalse(ProjectRootsWording.newRootFieldLabel.isEmpty)
        XCTAssertNotEqual(ProjectRootsWording.newRootFieldLabel, "/path/to/project")
        XCTAssertFalse(
            ProjectRootsWording.newRootFieldLabel.contains("/"),
            "the field's name should say what the field is for, not show a path"
        )
    }

    /// "Add" on its own is clear beside a field and unclear when read out of
    /// context, which is the only way VoiceOver ever reads it.
    func testAddLabelSaysWhatIsBeingAdded() {
        let label = ProjectRootsWording.addLabel.lowercased()

        XCTAssertTrue(label.contains("add"))
        XCTAssertTrue(label.contains("project root"))
    }

    /// Three controls sit on this pane. If any two were read the same way,
    /// keyboard-and-VoiceOver navigation through it would be guesswork.
    func testTheThreeControlsAreNamedDistinctly() {
        let names = [
            ProjectRootsWording.removeLabel(for: "/Users/dev/proj"),
            ProjectRootsWording.newRootFieldLabel,
            ProjectRootsWording.addLabel,
        ]

        XCTAssertEqual(Set(names).count, names.count, "two controls are read identically: \(names)")
    }
}
