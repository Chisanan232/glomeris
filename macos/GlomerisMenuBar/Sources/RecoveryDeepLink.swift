//
//  RecoveryDeepLink.swift
//  GlomerisMenuBar
//
//  HORO-1508 AC 2: "Review & recover" opens the Recovery surface, with the context
//  the alert was about.
//
//  ---------------------------------------------------------------------
//  The one place that names an answer
//  ---------------------------------------------------------------------
//  Everywhere else in this app the three answers are opaque tokens read from
//  `PressureStatusReport.responses`, precisely so a fourth cannot be invented.
//  This file is the exception, and it has to be: which answer *opens a window* is
//  a consequence in the user interface, not a decision the CLI can make for it.
//  Something has to know that one of the three means "show me".
//
//  So it is named once, here, and `RecoveryDeepLinkTests` asserts the CLI still
//  publishes it. If Rust renames the token, that test fails — instead of the
//  button quietly becoming inert, which is the failure this arrangement exists to
//  prevent: a user pressing "Review & recover" and having nothing happen, with no
//  error anywhere, because a string comparison silently stopped matching.
//
//  ---------------------------------------------------------------------
//  It opens a screen. It does not recover anything
//  ---------------------------------------------------------------------
//  Crossing a threshold must never start destructive work, and pressing a button
//  on a notification must not either. This deep link carries figures and opens a
//  surface; the run is started by a human on that surface, through the same
//  policy and executor path every other run takes.
//

import Foundation
import SwiftUI

/// Everything the Recovery surface needs to open on the alert the user answered.
///
/// A value, not a reference to the monitor: the window renders the state as it was
/// when the alert was answered, and a report that changed underneath it while the
/// user was reading would silently rewrite what they had been told.
struct RecoveryDeepLinkContext: Equatable {
    /// The episode this arrived from, when it arrived from one. `nil` for the
    /// in-app route — Recovery can be opened without an alert, and a context that
    /// claimed an episode id in that case would be inventing one.
    let episodeId: UInt64?

    /// Where the disk was, percent used, as the CLI measured it for this answer.
    let currentUsedPercent: Double

    /// Free space, in the CLI's own rendering.
    let currentFreeHuman: String

    /// What the user asked to be told at, in the CLI's own wording. Carried so the
    /// surface can say *why it is open* — and so the two percentages arrive
    /// together, which is the only way a reader can see they are different things.
    let notifyAtDescription: String

    /// Where a run would stop, in the CLI's own wording.
    let goalDescription: String

    /// The same goal as a whole percentage the Recovery card's control can hold,
    /// through the conversion rule that card already documents
    /// (`RecoverySectionView.storedDefaultGoal`). `nil` when the CLI's figure has
    /// no representable answer, in which case the card keeps its own default
    /// rather than being seeded with a guess.
    let goalUsedPercent: Int?

    /// What an automatic run already did during this episode, or `nil` if it had
    /// none (HORO-1510).
    ///
    /// The window this seeds opens with a reassurance, and on an opted-in Mac the
    /// reassurance can be false: the user is on this screen because a bounded run
    /// already happened and did not finish the job. Passed rather than defaulted
    /// for the reason `PressureBanner.automaticRun` gives.
    let automaticRun: UnpromptedRecoveryOutcome?

    init(report: PressureStatusReportDto, automaticRun: UnpromptedRecoveryOutcome?) {
        self.automaticRun = automaticRun
        episodeId = report.episode?.episodeId
        currentUsedPercent = report.current.usedPercent
        currentFreeHuman = report.current.freeHuman
        notifyAtDescription = report.notifyAtDescription
        goalDescription = report.defaultGoal.description
        // Deliberately the Recovery card's own conversion, not a second one. The
        // card rounds toward the disk staying fuller and clamps to what its
        // control can express, and it says why; a deep link that seeded the
        // control by a different rule would show the user a goal their setting
        // does not name, on the one screen where the number is about to be acted
        // on.
        goalUsedPercent = RecoverySectionView.storedDefaultGoal(
            usedPercent: report.defaultGoal.usedPercent
        )
    }
}

/// Which token opens Recovery, and the context to open it with.
enum RecoveryDeepLink {
    /// The answer that means "show me the Recovery screen".
    ///
    /// The single literal in this app that names a member of
    /// `EpisodeResponse::as_str`'s set for a reason other than wording it.
    /// `GlomerisVocabulary.episodeResponse` names all three, but only to supply
    /// their titles; this one decides behaviour, so it is asserted against the
    /// CLI's published set rather than trusted.
    static let reviewToken = "review_and_recover"

    /// Whether this answer should bring the Recovery surface forward.
    ///
    /// A function rather than an equality check at each call site, so there is one
    /// place to read and one place a future second "open something" answer would
    /// be added.
    static func opensRecovery(_ responseToken: String) -> Bool {
        responseToken == Self.reviewToken
    }
}

/// Brings the Recovery surface forward on the context an alert was answered with.
///
/// A protocol for two reasons. The monitor must be testable without a window
/// server — `MenuBarExtra` has no programmatic-open API, so the real implementation
/// is an AppKit window, and a test may not put one on screen. And the monitor has
/// no business knowing what a window is: it records answers through the CLI, and
/// hands the one answer that means "show me" to whatever can show it.
@MainActor
protocol RecoveryDeepLinkOpening: AnyObject {
    /// `nil` context means the surface should open anyway, without the pressure
    /// header — the disk could not be read for this answer.
    ///
    /// Optional rather than a precondition on purpose. The alternative is that a
    /// user presses "Review & recover" and *nothing happens*, which is the one
    /// outcome this whole arrangement exists to prevent; and the Recovery card
    /// reads the disk for itself when it appears, so a surface opened without a
    /// header is still useful rather than blank.
    func openRecovery(_ context: RecoveryDeepLinkContext?)
}
