//
//  ProjectRootsStore.swift
//  GlomerisMenuBar
//
//  HORO-1067: the project-roots preference — a list of folder paths the
//  user wants Glomeris to scope Cargo/Node detection to, matching the
//  Rust CLI's existing `--project-root` flag. This is product/UI state,
//  not policy state: it lives in UserDefaults, never in a new Rust config
//  file, and GlomerisClient/the Rust core need not know it exists. See
//  the standing project rule in GlomerisMenuBarApp.swift — this store
//  only persists paths and formats CLI arguments; it makes no
//  policy/evidence/action decisions.
//
//  Not wired into any detect/explain/execute call site yet — that is
//  later UI tickets' job (C4-C8). This ticket only needs the preference
//  to exist, persist, be observable/testable, and produce the correct
//  `--project-root` argument shape for those tickets to consume.
//

import Foundation

/// Reads and writes the configured list of project-root paths, and
/// formats them as repeated `--project-root <path>` arguments for
/// `GlomerisClient.run(_:)`.
///
/// Backed by a dedicated `UserDefaults(suiteName:)` instance rather than
/// `.standard` so this app's preference state is namespaced under its own
/// bundle identifier and doesn't collide with anything else reading
/// `.standard` in the same process. Every read goes straight to
/// `UserDefaults` — there is no in-memory cache — so an add/remove takes
/// effect on the very next read, in this process or a fresh one, with no
/// stale-list bug across app restarts.
struct ProjectRootsStore {
    private static let suiteName = "dev.glomeris.GlomerisMenuBar"
    private static let defaultsKey = "projectRoots"

    private let defaults: UserDefaults

    init(defaults: UserDefaults? = nil) {
        self.defaults = defaults ?? UserDefaults(suiteName: Self.suiteName) ?? .standard
    }

    /// The currently configured project-root paths, in the order they
    /// were added.
    var roots: [String] {
        defaults.stringArray(forKey: Self.defaultsKey) ?? []
    }

    /// Adds `path` to the configured roots. A no-op if `path` is already
    /// present, so the same root is never stored twice.
    func addRoot(_ path: String) {
        var current = roots
        guard !current.contains(path) else { return }
        current.append(path)
        defaults.set(current, forKey: Self.defaultsKey)
    }

    /// Removes `path` from the configured roots, if present.
    func removeRoot(_ path: String) {
        let current = roots
        defaults.set(current.filter { $0 != path }, forKey: Self.defaultsKey)
    }

    /// The configured roots formatted as repeated `--project-root <path>`
    /// arguments, ready to append to the `arguments` array passed to
    /// `GlomerisClient.run(_:)`. Empty when no roots are configured.
    var commandLineArguments: [String] {
        roots.flatMap { ["--project-root", $0] }
    }
}
