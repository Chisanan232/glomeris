//
//  ProjectRootsPreferencesView.swift
//  GlomerisMenuBar
//
//  HORO-1067: a simple list with add/remove for the project-roots
//  preference, backed by ProjectRootsStore. This was presentation only at
//  first — see the standing project rule in GlomerisMenuBarApp.swift — and
//  did not spawn GlomerisClient; that changed with HORO-1629 below.
//
//  HORO-1325 gave this pane's controls accessible names. It was written
//  before the later UI tickets established that idiom, and so was the one
//  view in the target shipping an icon-only button and a placeholder-only
//  field with nothing for VoiceOver to read.
//
//  HORO-1629 (ADR-0001 §11/§16.1, option 1): adding a folder now runs the
//  bounded, one-level `check-project-root` CLI check first, and shows its
//  server-authored warning when the folder looks like it holds several
//  projects rather than being one itself — the exact "added ~/code,
//  expected one repo" misconfiguration this ticket exists to catch, at
//  the moment it is still cheap to fix (before the folder is ever added
//  as a `--project-root`). This view still makes no policy/evidence
//  decision of its own: the wording shown is exactly what the CLI
//  returned, never composed here.
//

import SwiftUI

/// The spoken names for this pane's controls (HORO-1325).
///
/// A separate type for the same reason `AutopilotWording` is one: a label
/// nobody can call is a label nobody can assert, and the defect this closes
/// was an icon-only button whose name existed only in the mind of whoever
/// chose the glyph. `ProjectRootsPreferencesViewTests` pins these.
enum ProjectRootsWording {
    /// Names the action *and* its target. "Remove" alone would be read
    /// identically on every row, which on a list of paths is the same as
    /// saying nothing: the whole question a VoiceOver user has here is
    /// *which* root this button applies to.
    static func removeLabel(for path: String) -> String {
        "Remove project root \(path)"
    }

    /// The field's name, which is not the same thing as its placeholder.
    /// `/path/to/project` is an example of what to type; it does not say what
    /// the field is for, and a placeholder also disappears the moment
    /// anything is typed.
    static let newRootFieldLabel = "New project root path"

    /// "Add" is unambiguous on screen, beside a field, and ambiguous when
    /// read on its own.
    static let addLabel = "Add project root"
}

struct ProjectRootsPreferencesView: View {
    private let store: ProjectRootsStore
    private let client: GlomerisClient
    @State private var roots: [String]
    @State private var newRootPath: String = ""
    /// Server-authored warning from the most recent `check-project-root`
    /// call, or `nil` when there is nothing to show (clean, undetermined,
    /// or the check could not run at all — HORO-1629 §11/§16.1).
    @State private var warningMessage: String?

    init(store: ProjectRootsStore = ProjectRootsStore(), client: GlomerisClient = GlomerisClient()) {
        self.store = store
        self.client = client
        _roots = State(initialValue: store.roots)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Project Roots")
                .font(.headline)

            List {
                ForEach(roots, id: \.self) { root in
                    HStack {
                        Text(root)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Button {
                            removeRoot(root)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(ProjectRootsWording.removeLabel(for: root))
                    }
                }
            }
            .frame(minHeight: 120)

            if let warningMessage {
                Text(warningMessage)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel(warningMessage)
            }

            HStack {
                TextField("/path/to/project", text: $newRootPath)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel(ProjectRootsWording.newRootFieldLabel)
                Button("Add") {
                    let pathToAdd = newRootPath
                    Task { await addRoot(pathToAdd) }
                }
                .disabled(newRootPath.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityLabel(ProjectRootsWording.addLabel)
            }
        }
        .padding()
        .frame(width: 360, height: 240)
    }

    /// Runs the bounded, one-level `check-project-root` check (HORO-1629
    /// §11/§16.1, ADR-0001 option 1) before persisting `path`, then adds it
    /// regardless of the outcome: this is a warning, not a gate — the user
    /// may genuinely mean to scope detection to a folder that also happens
    /// to hold sibling directories, and ADR-0001 §11 never authorizes
    /// refusing the add.
    private func addRoot(_ path: String) async {
        let trimmed = path.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        warningMessage = await checkProjectRootWarning(for: trimmed)
        store.addRoot(trimmed)
        roots = store.roots
        newRootPath = ""
    }

    /// The server-authored warning for `path`, or `nil` when there is none
    /// to show — a clean root, an ordinary folder, an undetermined read
    /// (no founder-approved wording exists for that case; see ADR-0001
    /// §16.1's open AC5 question), or a `GlomerisClient` failure. This view
    /// never composes or infers wording of its own; it only ever shows
    /// exactly what the CLI returned.
    private func checkProjectRootWarning(for path: String) async -> String? {
        do {
            let result = try await client.run(
                ["check-project-root", path, "--json"],
                outputType: CheckProjectRootReportDto.self,
                progressType: EmptyProgressDto.self
            )
            return result.output.warning
        } catch {
            return nil
        }
    }

    private func removeRoot(_ path: String) {
        store.removeRoot(path)
        roots = store.roots
        warningMessage = nil
    }
}

#Preview {
    ProjectRootsPreferencesView()
}
