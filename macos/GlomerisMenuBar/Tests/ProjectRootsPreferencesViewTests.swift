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

    // MARK: - HORO-1629 follow-up: addRoot's check-before-add flow
    //
    // The static-wording tests above never drove `addRoot`/
    // `checkProjectRootWarning` at all — this is the gap the round-1 review
    // flagged. These tests follow `CandidatesSectionViewTests
    // .testGlomerisClientInvokesOnProgressPerNDJSONLine`'s pattern: compile
    // the tiny fixture helper once, point a `GlomerisClient` at it with
    // `executableURL:`, and drive the real client-backed code path rather
    // than a mock.

    /// Builds (compiling once, if needed) a `GlomerisClient` whose every
    /// invocation is the fixture helper, so a test can dictate exactly what
    /// `check-project-root --json` "returns" via the argv protocol:
    /// `[exitCode, stdout, stderr]`.
    private func fixtureClient() throws -> GlomerisClient {
        let binaryURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-client-fixture-helper-v4")
        if !FileManager.default.isExecutableFile(atPath: binaryURL.path) {
            let sourceURL = URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()
                .appendingPathComponent("Fixtures/glomeris_fixture_helper.c")
            let clang = Process()
            clang.executableURL = URL(fileURLWithPath: "/usr/bin/clang")
            clang.arguments = ["-O0", "-o", binaryURL.path, sourceURL.path]
            try clang.run()
            clang.waitUntilExit()
        }
        return GlomerisClient(executableURL: binaryURL)
    }

    /// AC1/"option 1" of this ticket: when `check-project-root` reports
    /// `parent_of_projects`, `checkProjectRootWarning` returns the exact
    /// ADR-0001 §16.1 warning string verbatim.
    ///
    /// Only `checkProjectRootWarning`'s return value is asserted directly —
    /// not `view.warningMessage` after calling `addRoot` — because `@State`
    /// has no stable storage location outside a real SwiftUI render graph:
    /// a write made inside an `await`ed call does not reliably read back
    /// through a second, independent property access on the same struct
    /// value (confirmed empirically: a direct call to
    /// `checkProjectRootWarning` returns the right value while
    /// `view.warningMessage`, read right after `addRoot` made that exact
    /// assignment, comes back `nil`). The decision logic under test —
    /// which string `checkProjectRootWarning` computes for which CLI
    /// outcome — is exercised here with no SwiftUI storage in the loop;
    /// `addRoot`'s own contract (assign unconditionally, then add
    /// unconditionally) is checked separately below, structurally, the same
    /// way this file already pins other call-site shapes
    /// (`testTheIncompleteDiscoveryAdvisoryIsAdditiveAndNeverReplacesTheRows`
    /// in `CandidatesSectionViewTests` is the sibling precedent).
    @MainActor
    func testCheckProjectRootWarningReturnsTheADRStringForParentOfProjects() async throws {
        let warning = "This folder appears to contain multiple projects rather than being a " +
            "project root. Glomeris checks only the selected folder and does not " +
            "automatically scan nested projects. Add each project separately to " +
            "include its build artifacts."
        let json = #"""
        {"path":"/Users/dev/code","status":"parent_of_projects","warning":"\#(warning)","matched_children":["repo-a"]}
        """#
        // `addRoot`/`checkProjectRootWarning` construct their own argv
        // (`["check-project-root", path, "--json"]`), so this test cannot hand
        // the fixture its stdout/exit via argv the way
        // `testGlomerisClientInvokesOnProgressPerNDJSONLine` does — it uses
        // the environment protocol instead, which the fixture answers from
        // before ever looking at argv (see the fixture's own doc comment).
        let client = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_STDOUT": json,
            "GLOMERIS_FIXTURE_EXIT": "0",
        ])
        let view = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: TestUserDefaults.inMemory()), client: client)

        let result = await view.checkProjectRootWarning(for: "/Users/dev/code")

        XCTAssertEqual(result, warning)
    }

    /// The normal case: a real project root reports `project_root`, no
    /// `warning` field at all, so there is nothing to show.
    @MainActor
    func testCheckProjectRootWarningIsNilForAProjectRoot() async throws {
        let json = #"{"path":"/Users/dev/my-app","status":"project_root"}"#
        let client = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_STDOUT": json,
            "GLOMERIS_FIXTURE_EXIT": "0",
        ])
        let view = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: TestUserDefaults.inMemory()), client: client)

        let result = await view.checkProjectRootWarning(for: "/Users/dev/my-app")

        XCTAssertNil(result, "a clean project root has nothing to warn about")
    }

    /// The gap the round-1 review called out directly: before this fix, a
    /// `GlomerisClient` failure (here, a non-zero exit — standing in for
    /// "the binary could not be spawned" / "exited abnormally") was
    /// swallowed into `nil` by the `catch` block, making a check that never
    /// ran indistinguishable from a check that ran and found nothing.
    /// `checkProjectRootWarning` must return something honest instead — the
    /// same fail-open shape AC4 forbids on the Rust side, closed here too.
    @MainActor
    func testCheckProjectRootWarningReturnsTheUnavailableMessageOnAClientFailure() async throws {
        let client = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_EXIT": "1",
        ])
        let view = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: TestUserDefaults.inMemory()), client: client)

        let result = await view.checkProjectRootWarning(for: "/Users/dev/unreachable")

        XCTAssertEqual(
            result, ProjectRootsWording.checkUnavailableMessage,
            "a failed check must not render identically to a confirmed-clean one"
        )
        XCTAssertNotNil(
            result,
            "nil here would be the fail-open bug: indistinguishable from the clean-root case above"
        )
    }

    /// Same failure class, different cause: stdout exits 0 but is not valid
    /// `CheckProjectRootReportDto` JSON. `GlomerisClient.run` throws
    /// `outputDecodingFailed` for this, and it must land in the same honest
    /// bucket as a spawn/exit failure, not in the clean-root bucket.
    @MainActor
    func testCheckProjectRootWarningReturnsTheUnavailableMessageOnUndecodableOutput() async throws {
        let client = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_STDOUT": "not valid json at all",
            "GLOMERIS_FIXTURE_EXIT": "0",
        ])
        let view = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: TestUserDefaults.inMemory()), client: client)

        let result = await view.checkProjectRootWarning(for: "/Users/dev/weird-output")

        XCTAssertEqual(result, ProjectRootsWording.checkUnavailableMessage)
    }

    /// The `"undetermined"` status specifically — a case the server
    /// completed (unlike the failure tests above) but chose to report no
    /// founder-approved wording for (ADR-0001 §16.1's open AC5). This must
    /// stay `nil`, distinct from `checkUnavailableMessage`: the server
    /// answered honestly that it could not determine the folder's shape,
    /// which is a different fact from the client never getting an answer at
    /// all, and the two must not collapse into the same string.
    @MainActor
    func testCheckProjectRootWarningIsNilNotTheFailureMessageForServerUndetermined() async throws {
        let json = #"{"path":"/Users/dev/missing","status":"undetermined","reason":"could not read /Users/dev/missing"}"#
        let client = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_STDOUT": json,
            "GLOMERIS_FIXTURE_EXIT": "0",
        ])
        let view = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: TestUserDefaults.inMemory()), client: client)

        let result = await view.checkProjectRootWarning(for: "/Users/dev/missing")

        XCTAssertNil(
            result,
            "a server-reported undetermined read is a different fact from a client-side failure " +
                "and must not borrow checkUnavailableMessage's wording"
        )
    }

    /// `addRoot` itself, end to end, using the real persisted store (never
    /// `@State`, so this reads back reliably): whatever
    /// `checkProjectRootWarning` decides, the root is still added — a
    /// warning is advisory, never a gate, per `addRoot`'s own doc comment
    /// and ADR-0001 §11. Covers both a reported warning and a client-side
    /// check failure, since "never blocks the user" matters most exactly
    /// when the check itself could not run.
    @MainActor
    func testAddRootPersistsTheRootRegardlessOfTheCheckOutcome() async throws {
        let parentOfProjectsJSON =
            #"{"path":"/Users/dev/code","status":"parent_of_projects","warning":"w","matched_children":["a"]}"#
        let warnedClient = try fixtureClient().withEnvironment([
            "GLOMERIS_FIXTURE_STDOUT": parentOfProjectsJSON,
            "GLOMERIS_FIXTURE_EXIT": "0",
        ])
        let warnedDefaults = TestUserDefaults.inMemory()
        let warnedView = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: warnedDefaults), client: warnedClient)
        await warnedView.addRoot("/Users/dev/code")
        XCTAssertEqual(
            ProjectRootsStore(defaults: warnedDefaults).roots, ["/Users/dev/code"],
            "a parent-of-projects warning must not block the add"
        )

        let failingClient = try fixtureClient().withEnvironment(["GLOMERIS_FIXTURE_EXIT": "1"])
        let failingDefaults = TestUserDefaults.inMemory()
        let failingView = ProjectRootsPreferencesView(
            store: ProjectRootsStore(defaults: failingDefaults), client: failingClient)
        await failingView.addRoot("/Users/dev/unreachable")
        XCTAssertEqual(
            ProjectRootsStore(defaults: failingDefaults).roots, ["/Users/dev/unreachable"],
            "a check that could not run must not block the user from adding the root either"
        )
    }

    /// Structural counterpart to the behavioral tests above, in the same
    /// spirit as `CandidatesSectionViewTests
    /// .testTheIncompleteDiscoveryAdvisoryIsAdditiveAndNeverReplacesTheRows`:
    /// `addRoot` must call `checkProjectRootWarning` and assign its result to
    /// `warningMessage` BEFORE calling `store.addRoot`, with no conditional
    /// between them that could skip the add on a particular outcome. A test
    /// that only exercises specific fixture outcomes (as above) cannot rule
    /// out a future `if result != nil { return }` or similar short-circuit
    /// for some outcome this suite did not happen to try; this pins the
    /// shape of the function itself.
    func testAddRootAssignsTheWarningThenAddsUnconditionally() throws {
        let source = try Self.strippedOfComments(Self.readSource("ProjectRootsPreferencesView.swift"))
        let body = try Self.functionBody(named: "func addRoot(_ path: String) async", in: source)

        let assignIndex = try XCTUnwrap(
            body.range(of: "warningMessage = await checkProjectRootWarning(for: trimmed)"),
            "addRoot no longer assigns checkProjectRootWarning's result to warningMessage: \(body)"
        )
        let addIndex = try XCTUnwrap(
            body.range(of: "store.addRoot(trimmed)"),
            "addRoot no longer calls store.addRoot(trimmed): \(body)"
        )
        XCTAssertLessThan(
            assignIndex.upperBound, addIndex.lowerBound,
            "the check must run and be assigned before the root is persisted"
        )

        // Between the two calls above, exactly the `warningMessage = ...` line
        // itself and nothing else — no `if`, `guard`, or `return` that could
        // make the add conditional on what the check found.
        let between = body[assignIndex.upperBound..<addIndex.lowerBound]
        for guardToken in ["if ", "guard ", "return"] {
            XCTAssertFalse(
                between.contains(guardToken),
                "a conditional appeared between the check and the add — the add is no longer " +
                    "unconditional: \(between)"
            )
        }
    }

    /// Anti-vacuity for the guard above: splicing a short-circuit back in
    /// between the two calls must actually trip the assertion, so a future
    /// rename of either call site cannot turn that guard into one that
    /// checks nothing.
    func testTheUnconditionalAddGuardWouldCatchAShortCircuitReturning() throws {
        let source = try Self.strippedOfComments(Self.readSource("ProjectRootsPreferencesView.swift"))
        let body = try Self.functionBody(named: "func addRoot(_ path: String) async", in: source)
        let regressed = body.replacingOccurrences(
            of: "warningMessage = await checkProjectRootWarning(for: trimmed)",
            with: "warningMessage = await checkProjectRootWarning(for: trimmed)\n"
                + "        if warningMessage != nil { return }"
        )
        XCTAssertNotEqual(regressed, body, "no assignment line to splice onto — addRoot changed shape")

        let assignIndex = try XCTUnwrap(regressed.range(of: "warningMessage = await checkProjectRootWarning"))
        let addIndex = try XCTUnwrap(regressed.range(of: "store.addRoot(trimmed)"))
        let between = regressed[assignIndex.upperBound..<addIndex.lowerBound]
        XCTAssertTrue(
            between.contains("if "),
            "the guard would not notice a short-circuit return between the check and the add"
        )
    }

    // MARK: - Source-inspection helpers (mirrors CandidatesSectionViewTests)

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }

    private static func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    /// `signature`'s function body, matched from its opening `{` to the
    /// closing brace at the function's own indentation (4 spaces, a type
    /// member) — the same convention `CandidatesSectionViewTests.rowViewBody`
    /// uses, so a brace nested inside the body (the `do`/`catch` in
    /// `checkProjectRootWarning`, if this were called on that function
    /// instead) is never mistaken for the function's own end.
    private static func functionBody(named signature: String, in source: String) throws -> String {
        let range = try XCTUnwrap(
            source.range(of: signature), "no `\(signature)` — renamed, or no longer a function")
        let rest = source[range.upperBound...]
        let end = try XCTUnwrap(rest.range(of: "\n    }\n"), "could not find the end of \(signature)")
        return String(rest[..<end.upperBound])
    }
}
