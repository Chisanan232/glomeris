//
//  PressureNotification.swift
//  GlomerisMenuBar
//
//  HORO-1508 AC 1, 2 and 6: the local macOS notification, its three buttons, and
//  the answer coming back.
//
//  ---------------------------------------------------------------------
//  Why this app raises it and the daemon does not
//  ---------------------------------------------------------------------
//  `osascript display notification` cannot carry buttons, and
//  `UNUserNotificationCenter` — which can — refuses to run outside an app bundle.
//  The monitor daemon is a bare launchd process, so the process that *notices*
//  pressure cannot be the process that *asks about* it. Rust records that a
//  notification is due; this file is the part that can put it on screen.
//
//  ---------------------------------------------------------------------
//  Two findings that shape the code, both measured
//  ---------------------------------------------------------------------
//  1. **The request identifier must include the raised count, not just the episode
//     id.** Re-posting a request with an identifier already delivered updates the
//     existing notification *in place, with no new banner*. Keyed on the episode id
//     alone, the reminder a snooze produces would silently show nothing — AC 3
//     inverted, invisibly. See `PressureBannerKey`.
//
//  2. **`requestAuthorization` must never be used as a gate.** Measured in an
//     ad-hoc-signed LSUIElement bundle: the first call returned `granted = false`
//     with `Code=1 "Notifications are not allowed for this application"` and *no
//     visible prompt*, while later runs of the same bundle delivered notifications
//     successfully with `alertSetting: 2`. A client that refused to post when
//     authorization reported failure would therefore refuse to post in a case where
//     posting works. So it is requested, its answer is *recorded*, and the request
//     is posted regardless; `center.add` is what actually decides.
//
//  ---------------------------------------------------------------------
//  Nothing here deletes anything
//  ---------------------------------------------------------------------
//  No action on this notification starts recovery. "Review & recover" opens a
//  screen; the other two adjust when the user is spoken to. The body says so, in
//  words, because a banner is read in a hurry. No action carries `.destructive`:
//  that styling means "this removes something", and none of them do.
//

import Foundation
import UserNotifications

// MARK: - What the notification says

/// The banner's text, identifiers and buttons, as pure values.
///
/// Separated from the delivery below so all of it is assertable: the identifier
/// rule, the buttons being the CLI's own answer set, and the body stating that
/// nothing has been deleted. Delivery itself needs a real notification centre,
/// which a test bundle does not have.
enum PressureNotificationContent {
    /// The category the three actions are registered under. One category, so a
    /// re-post updates rather than accumulating variants.
    static let categoryIdentifier = "DISK_PRESSURE"

    /// Groups an episode's banners together in Notification Centre, so a snooze
    /// and its reminder read as one conversation about one episode.
    static func threadIdentifier(episodeId: UInt64) -> String {
        "dev.glomeris.pressure.episode.\(episodeId)"
    }

    /// The request identifier — episode id **and** banners already raised.
    ///
    /// Both halves are load-bearing. The episode id keeps a hovering disk from
    /// stacking banners; the raised count is what makes the post-snooze reminder a
    /// *new* banner rather than an in-place update of one already dismissed.
    static func requestIdentifier(for key: PressureBannerKey) -> String {
        "dev.glomeris.pressure.\(key.episodeId).\(key.notificationsRaised)"
    }

    /// e.g. `"Disk is 91.0% used"`. The same `%.1f%% used` rendering the Recovery
    /// card and the history rows use, so one number does not appear in two
    /// precisions across the app.
    static func title(for banner: PressureBanner) -> String {
        String(format: "Disk is %.1f%% used", banner.currentUsedPercent)
    }

    /// The body, in the order a hurried reader needs it: what is true now, why
    /// they are being told, where recovery would stop, and that nothing has
    /// happened yet.
    ///
    /// The threshold and the goal are named as two separate things. Collapsing
    /// them into one figure is the specific misreading this campaign exists to
    /// prevent: one is when to speak, the other is where a run stops.
    static func body(for banner: PressureBanner) -> String {
        "\(banner.freeHuman) free. You asked to be alerted at "
            + "\(banner.notifyAtDescription); your recovery goal is \(banner.goalDescription). "
            + "Nothing has been deleted."
    }

    /// The buttons, built from the tokens the CLI published.
    ///
    /// Titles come from `GlomerisVocabulary.episodeResponse`, so the wording the
    /// notification shows is the wording every other surface shows for the same
    /// answer — and a token this app has no wording for still gets a button
    /// rather than disappearing, because the CLI will accept it.
    ///
    /// The identifier *is* the token. That is what lets the answer come back
    /// without a second mapping to keep in step, and it is why no list of answers
    /// is written down in this file.
    ///
    /// `.foreground` only on the one that opens a window, because that is the only
    /// one that needs the app in front. The others are recorded without disturbing
    /// whatever the user is doing — which is most of the point of a "later" button.
    static func actions(responseTokens: [String]) -> [UNNotificationAction] {
        responseTokens.map { token in
            UNNotificationAction(
                identifier: token,
                title: GlomerisVocabulary.episodeResponse(token).title,
                options: RecoveryDeepLink.opensRecovery(token) ? [.foreground] : []
            )
        }
    }

    static func category(responseTokens: [String]) -> UNNotificationCategory {
        UNNotificationCategory(
            identifier: Self.categoryIdentifier,
            actions: Self.actions(responseTokens: responseTokens),
            intentIdentifiers: [],
            options: []
        )
    }

    static func request(for banner: PressureBanner) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = Self.title(for: banner)
        content.body = Self.body(for: banner)
        content.categoryIdentifier = Self.categoryIdentifier
        content.threadIdentifier = Self.threadIdentifier(episodeId: banner.key.episodeId)
        // No sound. A disk filling up is not an event that needs to interrupt a
        // call, and a silent banner is the difference between a feature a user
        // keeps enabled and one they turn off.
        content.sound = nil
        return UNNotificationRequest(
            identifier: Self.requestIdentifier(for: banner.key),
            content: content,
            // Immediately. There is nothing to schedule: the condition is already
            // true, which is why Rust recorded that a banner was owed.
            trigger: nil
        )
    }

    /// Which token a delivered action identifier means, or `nil` for one this app
    /// must not turn into an answer.
    ///
    /// Three cases, and the third is the one worth stating. Tapping the banner
    /// body (`UNNotificationDefaultActionIdentifier`) means "show me", so it maps
    /// to the answer that opens Recovery — provided the CLI published it.
    /// *Dismissing* the banner is deliberately not an answer: silence is not a
    /// choice, and recording it as "ignore this alert" would put words in the
    /// user's mouth and close an episode they never answered. The in-app surface
    /// still shows the pending question afterwards (AC 6).
    static func answer(forActionIdentifier identifier: String, published: [String]) -> String? {
        if published.contains(identifier) {
            return identifier
        }
        if identifier == UNNotificationDefaultActionIdentifier {
            return published.first(where: RecoveryDeepLink.opensRecovery)
        }
        return nil
    }
}

// MARK: - Delivery

/// `PressureBannerRaising` over the real `UNUserNotificationCenter`.
///
/// `center` is injected with no default on purpose. `UNUserNotificationCenter
/// .current()` traps in a process with no app bundle, so a defaulted parameter
/// would make this type unconstructible in a test target rather than merely
/// untested there — and the tests assert `PressureNotificationContent` above,
/// which holds every claim worth pinning and needs no notification centre at all.
@MainActor
final class PressureNotificationCenterBanner: NSObject, PressureBannerRaising {
    /// What the last authorization request said, for display rather than for
    /// branching. See the file header: a `false` here has been measured on a
    /// bundle that then delivered notifications perfectly well.
    private(set) var authorizationNote: String?

    /// What went wrong with the last attempt to post, if anything.
    private(set) var lastFailure: String?

    /// The tokens the CLI published, kept so an action identifier coming back can
    /// be checked against the set that was actually offered rather than against a
    /// list in Swift.
    private var published: [String] = []

    private var hasRequestedAuthorization = false

    var onAnswer: ((String) -> Void)?

    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter) {
        self.center = center
        super.init()
    }

    /// Registers the category and takes the delegate.
    ///
    /// Both happen before any banner is posted — a delegate installed afterwards
    /// misses the response for a notification the user was quick about, and a
    /// category registered afterwards yields a banner with no buttons at all.
    func prepare(responseTokens: [String]) {
        guard published != responseTokens || center.delegate !== self else {
            // Nothing changed; re-registering on every poll would be pointless
            // work against a system service.
            return
        }
        published = responseTokens
        center.delegate = self
        center.setNotificationCategories([
            PressureNotificationContent.category(responseTokens: responseTokens)
        ])
    }

    func raise(_ banner: PressureBanner) async -> Bool {
        prepare(responseTokens: banner.responseTokens)
        await requestAuthorizationOnce()

        do {
            try await center.add(PressureNotificationContent.request(for: banner))
            lastFailure = nil
            return true
        } catch {
            // Reported, and reported as `false`, so the episode keeps owing a
            // banner rather than being recorded as spoken to.
            lastFailure = error.localizedDescription
            return false
        }
    }

    /// Asked once per launch, recorded, and never branched on.
    private func requestAuthorizationOnce() async {
        guard !hasRequestedAuthorization else { return }
        hasRequestedAuthorization = true
        do {
            let granted = try await center.requestAuthorization(options: [.alert])
            authorizationNote =
                granted
                ? nil
                : "macOS reported that notifications are not allowed for Glomeris. Alerts are "
                    + "still attempted, and can be enabled in System Settings › Notifications."
        } catch {
            authorizationNote =
                "macOS would not say whether notifications are allowed for Glomeris: "
                + error.localizedDescription
        }
    }

    /// The main-actor half of the delegate callbacks below.
    fileprivate func deliver(actionIdentifier: String) {
        guard
            let token = PressureNotificationContent.answer(
                forActionIdentifier: actionIdentifier,
                published: published
            )
        else { return }
        onAnswer?(token)
    }
}

extension PressureNotificationCenterBanner: UNUserNotificationCenterDelegate {
    /// `nonisolated` because the protocol requirement is, and the hop is explicit:
    /// only the action identifier — a `String` — crosses, rather than the
    /// non-`Sendable` response object.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let actionIdentifier = response.actionIdentifier
        completionHandler()
        Task { @MainActor [weak self] in
            self?.deliver(actionIdentifier: actionIdentifier)
        }
    }

    /// Shows the banner even when Glomeris is the active application, which it is
    /// whenever its popover or a Settings window is open. Without this, the one
    /// user who is actively looking at the app is the one who never sees the alert.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list])
    }
}
