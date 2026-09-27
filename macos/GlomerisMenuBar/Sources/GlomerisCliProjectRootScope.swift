//
//  GlomerisCliProjectRootScope.swift
//  GlomerisMenuBar
//
//  HORO-1501: which CLI subcommands the app attaches the configured project
//  roots to, in one place.
//
//  The defect this exists to remove. `StatusHealthSectionView` built its
//  argument vector as `["status", "--json"] + projectRootsStore
//  .commandLineArguments`, and `glomeris status` takes no project-root
//  argument — it reports free space and daemon-independent disk figures and
//  runs no detectors, so a root cannot change its answer. Every user with at
//  least one configured root therefore saw `unrecognized argument` where the
//  Status card should have been. Measured on a DogFood build with three roots
//  configured; with none configured the same code works, which is why it
//  survived review and manual use alike.
//
//  It survived twice over, because the CLI used to accept the flag silently.
//  HORO-1322 made `detect`, `explain` and `status` reject unknown tokens the way
//  the deleting commands already did, which is correct — and which turned a call
//  site that had always been wrong into a visible failure. Nothing compared the
//  client's invocations against the CLI's argument contract, so there was
//  nowhere for that to be caught.
//
//  Why a table rather than one deleted argument. Fourteen call sites across nine
//  views build argument vectors, eight of them concatenating roots. Any of them
//  can be wrong in exactly this way, and half of them name a command that does
//  take roots — so the reviewer's question at each site is "does this command
//  accept them?", a question about the CLI being asked inside a SwiftUI view.
//  Asked once here instead, it is answered where the arguments are produced
//  rather than at each site that consumes them, which is the standing project
//  rule in GlomerisMenuBarApp.swift: this client is a thin presenter and holds
//  no judgment of its own.
//
//  This table is the APP's contract and is deliberately narrower than the CLI's
//  capability. `glomeris::cli::extract_project_roots` is called by seven command
//  handlers in src/main.rs; four of them are in the set below. The other three
//  are not, and not by omission:
//
//    * `clean` and `free` accept roots, and the app never invokes them at all.
//    * `autopilot run` accepts roots — `autopilot show`, `enable` and `revoke`,
//      the three the app does invoke, reject extra arguments outright. Were the
//      app to gain an `autopilot run` call it would still send none: the stored
//      grant's own kinds and limits decide what that command may consider, and
//      silently narrowing a grant the user wrote down is not this table's
//      decision to make.
//
//  `scripts/check-app-cli-invocations-match-cli-contract.sh` is the other half,
//  and the reason this comment can be trusted: it extracts that set of seven
//  from the Rust sources, fails if this table is not a subset of it or if the
//  three absences above stop being the whole difference, and fails if any
//  argument vector in the Swift sources attaches roots by hand instead of
//  through here.
//

import Foundation

/// Decides whether a `glomeris` invocation is one the configured project roots
/// belong on.
///
/// Consulted through ``ProjectRootsStore/scoped(_:)`` and
/// ``ProjectRootsStore/projectRootArguments(forCommand:)`` rather than directly.
/// Those two are the only ways to obtain the roots in argument form, and both
/// route through here, so a caller cannot attach roots without naming the
/// command they are attaching them to — which is the one question the old
/// unconditional `commandLineArguments` property let every caller skip.
enum GlomerisCliProjectRootScope {
    /// The subcommands this app scopes to the configured project roots, keyed by
    /// the first token of the argument vector.
    ///
    /// Each entry is a command whose *result* the roots change:
    ///
    /// * `detect` — the roots are the extra directories it walks.
    /// * `explain` — resolves a candidate, so it must walk the same set, or a
    ///   candidate `detect` found becomes one `explain` cannot see.
    /// * `llm-plan` — the roots bound what may leave the machine for a BYOK
    ///   provider (HORO-1298), so dropping them here would widen egress.
    /// * `execute` — acts on a candidate, and must resolve it the same way
    ///   `explain` did or it would act on a resource other than the one the user
    ///   was shown.
    ///
    /// Everything else the app runs — `status`, `daemon status`, `llm-check`,
    /// `history`, `actions history`, `autopilot show`/`enable`/`revoke` — takes
    /// no project-root argument and rejects one.
    static let rootScopedCommands: Set<String> = [
        "detect",
        "explain",
        "llm-plan",
        "execute",
    ]

    /// Whether `command` is one the configured roots belong on.
    static func accepts(command: String) -> Bool {
        rootScopedCommands.contains(command)
    }

    /// Whether `arguments` names a command the configured roots belong on.
    ///
    /// Keyed on the first token only. No two-token command — `daemon status`,
    /// `actions history`, `autopilot show` — is in the set above, so a
    /// subcommand cannot change the answer; and keying on the first token means
    /// an unrecognised command gets no roots rather than the benefit of the
    /// doubt.
    ///
    /// An empty vector is not a command and gets nothing. It is also not this
    /// type's business to reject: whatever produced it is already broken, and
    /// answering "no roots" leaves that failure where it happened instead of
    /// turning it into a trap here.
    static func acceptsProjectRoots(_ arguments: [String]) -> Bool {
        guard let command = arguments.first else { return false }
        return accepts(command: command)
    }
}
