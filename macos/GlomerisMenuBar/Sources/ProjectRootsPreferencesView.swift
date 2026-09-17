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

import SwiftUI

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
                    }
                }
            }
            .frame(minHeight: 120)

            HStack {
                TextField("/path/to/project", text: $newRootPath)
                    .textFieldStyle(.roundedBorder)
                Button("Add") {
                    addRoot(newRootPath)
                }
                .disabled(newRootPath.trimmingCharacters(in: .whitespaces).isEmpty)
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
