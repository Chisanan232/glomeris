//
//  ProjectRootsPreferencesView.swift
//  GlomerisMenuBar
//
//  HORO-1067: a simple list with add/remove for the project-roots
//  preference, backed by ProjectRootsStore. This is presentation only —
//  see the standing project rule in GlomerisMenuBarApp.swift. It does not
//  spawn GlomerisClient or wire into any detect/explain/execute call
//  site; that is later UI tickets' job (C4-C8).
//
//  HORO-1325 gave this pane's controls accessible names. It was written
//  before the later UI tickets established that idiom, and so was the one
//  view in the target shipping an icon-only button and a placeholder-only
//  field with nothing for VoiceOver to read.
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
    @State private var roots: [String]
    @State private var newRootPath: String = ""

    init(store: ProjectRootsStore = ProjectRootsStore()) {
        self.store = store
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

            HStack {
                TextField("/path/to/project", text: $newRootPath)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel(ProjectRootsWording.newRootFieldLabel)
                Button("Add") {
                    addRoot(newRootPath)
                }
                .disabled(newRootPath.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityLabel(ProjectRootsWording.addLabel)
            }
        }
        .padding()
        .frame(width: 360, height: 240)
    }

    private func addRoot(_ path: String) {
        let trimmed = path.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        store.addRoot(trimmed)
        roots = store.roots
        newRootPath = ""
    }

    private func removeRoot(_ path: String) {
        store.removeRoot(path)
        roots = store.roots
    }
}

#Preview {
    ProjectRootsPreferencesView()
}
