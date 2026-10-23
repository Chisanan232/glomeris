//
//  AppVersionLabel.swift
//  GlomerisMenuBar
//
//  HORO-2170: the popover showed no app version anywhere, so a tester could
//  not tell which build they were looking at. The CLI card deliberately shows
//  a content hash and no CLI version (HORO-1466: the version string is stamped
//  from the crate version and cannot tell two binaries apart); that stays. This
//  is the APP's own `CFBundleShortVersionString`, read from the bundle, with no
//  process spawned and no CLI involved. Presentation only.
//

import Foundation

enum AppVersionLabel {
    /// The bundle's marketing version, or nil when it is absent, blank or an
    /// unexpanded build setting (`$(MARKETING_VERSION)`), so the header never
    /// prints a placeholder as if it were a version.
    static func version(from info: [String: Any]?) -> String? {
        guard let raw = info?["CFBundleShortVersionString"] as? String else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty || trimmed.hasPrefix("$(") ? nil : trimmed
    }

    /// The version of the running app, or nil when it cannot be read.
    static var running: String? { version(from: Bundle.main.infoDictionary) }

    /// What VoiceOver reads for the header: the title, then the version.
    static func spokenLabel(title: String, version: String?) -> String {
        guard let version else { return title }
        return "\(title), version \(version)"
    }
}
