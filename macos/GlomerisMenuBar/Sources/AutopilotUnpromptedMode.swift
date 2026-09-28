//
//  AutopilotUnpromptedMode.swift
//  GlomerisMenuBar
//
//  HORO-1510 AC 2: automatic mode is opt-in and *visibly distinct* from
//  notification-only mode.
//
//  ---------------------------------------------------------------------
//  Why a type rather than a sentence in each card
//  ---------------------------------------------------------------------
//  Two surfaces have to say which mode is in force — the Autopilot pane, where
//  it is chosen, and the pressure-alert card in the popover, where its
//  consequence arrives. Written twice they would drift, and the direction a
//  drift matters in is the dangerous one: a card still saying "you will be asked
//  first" on a Mac that no longer asks.
//
//  So the wording lives here once, as a pure mapping a test can drive, and both
//  cards render it.
//
//  ---------------------------------------------------------------------
//  It reads authority; it does not compute it
//  ---------------------------------------------------------------------
//  See the standing project rule at the top of GlomerisMenuBarApp.swift. The
//  case that means "this Mac may delete without being asked" is decided by
//  `startsUnprompted`, which Rust computed in
//  `AutopilotEnvelope::starts_unprompted`. `respondToAlerts` is consulted only
//  to tell the two *inert* cases apart — off, versus set but dormant under a
//  revoked grant — because a settings pane that showed "off" for a preference
//  the user had chosen would read as revoking having silently cleared it.
//
//  Composing `enabled && respondToAlerts` here instead would make this app the
//  place that decides when it may act unasked. Both fields are reported
//  separately for exactly that reason.
//

import Foundation

/// Whether a recovery run can begin with nobody present, as the three states
/// that exist.
enum AutopilotUnpromptedMode: Equatable {
    /// Notification-only: the default, and the one every Mac is in until
    /// somebody says otherwise.
    case askedFirst

    /// Automatic: a pressure alert may start a bounded run.
    case startsOnPressure

    /// The preference is set and Autopilot is revoked, so nothing starts. Kept
    /// apart from ``askedFirst`` so that revoking never reads as having cleared
    /// a choice it deliberately keeps on file.
    case dormantWhileRevoked

    /// Reads the grant. `startsUnprompted` is the authority; the second field
    /// only separates the two states in which nothing happens.
    static func make(_ report: AutopilotEnvelopeDto) -> AutopilotUnpromptedMode {
        if report.startsUnprompted {
            return .startsOnPressure
        }
        return report.respondToAlerts ? .dormantWhileRevoked : .askedFirst
    }

    /// Whether this is the mode a user has opted in to. Distinct from "in
    /// force": a dormant grant was opted in to and still does nothing.
    var isOptedIn: Bool {
        switch self {
        case .askedFirst: return false
        case .startsOnPressure, .dormantWhileRevoked: return true
        }
    }

    /// Whether a run can actually begin unasked right now. The one property a
    /// caller deciding what will happen should read.
    var startsRunsUnprompted: Bool { self == .startsOnPressure }

    var title: String {
        switch self {
        case .askedFirst:
            return "You are asked first"
        case .startsOnPressure:
            return "Glomeris may start a run on its own"
        case .dormantWhileRevoked:
            return "Allowed, but dormant"
        }
    }

    var detail: String {
        switch self {
        case .askedFirst:
            return "A disk-pressure alert puts a notification on screen and waits. Nothing is "
                + "reclaimed until you choose Review & recover."
        case .startsOnPressure:
            return "A disk-pressure alert can start a bounded recovery run with nobody present, "
                + "inside the limits above. You are not asked first."
        case .dormantWhileRevoked:
            return "Autopilot is off, so nothing starts on its own. Your choice is kept, and "
                + "turning Autopilot on again restores it."
        }
    }

    /// Filled symbol for the mode that acts, outline for the two that do not —
    /// so the distinction is not carried by colour alone (campaign §14).
    var symbolName: String {
        switch self {
        case .askedFirst: return "hand.raised"
        case .startsOnPressure: return "bolt.badge.clock.fill"
        case .dormantWhileRevoked: return "pause.circle"
        }
    }

    var tone: GlomerisTone {
        switch self {
        case .askedFirst, .dormantWhileRevoked:
            // Neutral, not positive. Being asked first is the default, not an
            // achievement, and a dormant preference is not a fault.
            return .neutral
        case .startsOnPressure:
            // `.caution` and not `.critical`, for the reason
            // `AutopilotStatusViewModel` gives about the grant itself: this is
            // the feature working as designed, and red would teach a user that
            // their own decision is a problem.
            return .caution
        }
    }

    /// The short form for a row that has no space for `detail` — the popover's
    /// alert card, where the question on screen is what will happen next.
    var summary: String {
        switch self {
        case .askedFirst: return "You will be asked first"
        case .startsOnPressure: return "Autopilot may start a bounded run"
        case .dormantWhileRevoked: return "Autopilot is off, so you will be asked"
        }
    }

    /// What VoiceOver reads where the two `Text` views are one element.
    ///
    /// Stated rather than left to `.combine`, for the reason
    /// `AutopilotStatusViewModel.accessibilityLabel` documents: SwiftUI joins
    /// them with ", " and a listener hears a comma splice followed by a capital
    /// letter.
    var accessibilityLabel: String {
        SpokenLabel.compose([title, detail])
    }
}
