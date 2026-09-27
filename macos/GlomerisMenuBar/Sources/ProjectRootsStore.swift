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
//  HORO-1501: producing that shape is no longer unconditional. The flag is
//  formatted only for the commands that accept it, per
//  `GlomerisCliProjectRootScope` — because one of the call sites C4-C8 went on
//  to add was `status`, which takes no project root and exits 2 when handed
//  one, and nothing in this file's contract said it could not be asked.
//

import Foundation

/// Reads and writes the configured list of project-root paths, and formats them
/// as repeated `--project-root <path>` arguments for the `GlomerisClient.run(_:)`
/// invocations whose command accepts them.
///
/// Backed by `UserDefaults.standard`, which for a bundled app *is* the
/// domain named by its bundle identifier — so this app's preference state is
/// namespaced under its own identifier by construction, and a build with a
/// different identifier gets a different domain without anything here saying
/// so. Every read goes straight to `UserDefaults` — there is no in-memory
/// cache — so an add/remove takes effect on the very next read, in this
/// process or a fresh one, with no stale-list bug across app restarts.
///
/// HORO-1456: this used to read `UserDefaults(suiteName:) ?? .standard`
/// against the literal `dev.glomeris.GlomerisMenuBar`, and the comment above
/// claimed the suite was what namespaced it. It was not. Foundation returns
/// `nil` from `UserDefaults(suiteName:)` when handed the calling process's own
/// bundle identifier — logging "using your own bundle identifier as an
/// NSUserDefaults suite name does not make sense and will not work" — so in
/// the app the `?? .standard` fallback was always the operative branch. The
/// storage is unchanged; what changed is that the code now says which domain
/// it uses instead of arriving there through a failed initialiser.
struct ProjectRootsStore {
    private static let defaultsKey = "projectRoots"

    private let defaults: UserDefaults

    init(defaults: UserDefaults? = nil) {
        self.defaults = defaults ?? .standard
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

    /// `arguments` with the configured roots appended, when its leading token
    /// names a command that takes them; `arguments` unchanged otherwise.
    ///
    /// The intended way to build any invocation in this app, including the ones
    /// that receive no roots — `projectRootsStore.scoped(["status", "--json"])`
    /// reads as "the roots do not apply here, and something checked" where a
    /// bare `["status", "--json"]` only reads as "nobody attached them at this
    /// site today".
    ///
    /// HORO-1501. This replaced a `commandLineArguments` property that formatted
    /// the roots for any caller that asked, and the difference is where the
    /// question gets asked. `["status", "--json"] + projectRootsStore
    /// .commandLineArguments` is one token away from the four correct sites that
    /// look exactly like it, and the CLI rejects it — so the Status card failed
    /// for every user who had configured a root, which is to say for every user
    /// of the feature the roots exist for.
    ///
    /// Appends rather than inserts. `glomeris`'s own argument scanners
    /// (`extract_project_roots`, then a positional/flag loop per command) accept
    /// flags in any order, and the one command whose vector has a positional —
    /// `explain <resource-id>` — reads that positional before the flags.
    func scoped(_ arguments: [String]) -> [String] {
        guard let command = arguments.first else { return arguments }
        return arguments + projectRootArguments(forCommand: command)
    }

    /// The configured roots as repeated `--project-root <path>` arguments for
    /// `command`, or nothing at all when `command` does not take them.
    ///
    /// For the two call sites that build their vector through a separate pure
    /// function — `CandidateDetailView.buildExecuteArguments` and
    /// `AiProviderPreferencesView.payloadPreview` — where the roots sit in the
    /// middle of the vector and so cannot be appended by ``scoped(_:)``.
    ///
    /// Naming the command is not optional and the answer is not this method's:
    /// it comes from ``GlomerisCliProjectRootScope``, the same table
    /// ``scoped(_:)`` consults. There is deliberately no way left to obtain the
    /// roots in argument form without saying what they are for.
    func projectRootArguments(forCommand command: String) -> [String] {
        guard GlomerisCliProjectRootScope.accepts(command: command) else { return [] }
        return roots.flatMap { ["--project-root", $0] }
    }
}
