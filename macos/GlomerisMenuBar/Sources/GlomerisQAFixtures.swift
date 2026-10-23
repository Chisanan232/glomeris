//
//  GlomerisQAFixtures.swift
//  GlomerisMenuBar
//
//  HORO-2176: the DEV-QA synthetic-data mode that lets XCUITest drive the real
//  popover and candidates window over a deterministic result set.
//
//  The whole mode is the single seam below, and all of it is compiled only in a
//  DEBUG build. A Release build (the DEV-QA hand-off app and anything shipped)
//  contains none of these symbols or strings;
//  scripts/check-qa-fixtures-absent-from-release.sh proves it on a built binary.
//
//  What the seam does: when GLOMERIS_QA_FIXTURE_CLI names an existing executable,
//  `GlomerisClient` spawns THAT instead of the real `glomeris` for every command
//  (status polling, detect, explain, execute alike). The stand-in
//  (QAFixtures/glomeris-qa-fixture-cli.py) reads nothing from the machine, emits
//  candidates through the same JSON the real CLI emits, and refuses every
//  command that could act. So the Swift code under test is the production code,
//  the decoding is the production decoding, and nothing here classifies,
//  authorises or deletes anything: no policy logic and no new cleanup authority
//  are added to the app.
//

#if DEBUG
import Foundation
import SwiftUI

enum GlomerisQAFixtures {
    static let cliEnvironmentKey = "GLOMERIS_QA_FIXTURE_CLI"

    /// The stand-in CLI, only when explicitly requested with an absolute path to
    /// an executable file. A relative path or a missing file is ignored rather
    /// than guessed at, so a typo falls back to the real resolution instead of
    /// silently running something else.
    static func fixtureCLI(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL? {
        guard let path = environment[cliEnvironmentKey], path.hasPrefix("/"),
            FileManager.default.isExecutableFile(atPath: path)
        else { return nil }
        return URL(fileURLWithPath: path)
    }

    static var isActive: Bool { fixtureCLI() != nil }
}

/// The unmistakable "this is not your data" strip.
struct GlomerisQABanner: View {
    var body: some View {
        if GlomerisQAFixtures.isActive {
            Text("DEV-QA SYNTHETIC DATA — not your files; nothing here can be cleaned")
                .font(GlomerisDesign.captionFont.weight(.semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.purple)
                .accessibilityIdentifier("glomeris.qa.banner")
        }
    }
}
#else
import SwiftUI

/// Release builds draw nothing.
struct GlomerisQABanner: View {
    var body: some View { EmptyView() }
}
#endif
