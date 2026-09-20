//
//  GlomerisExecutableLocator.swift
//  GlomerisMenuBar
//
//  HORO-1295: decides which `glomeris` binary the app spawns.
//
//  See the standing project rule in GlomerisMenuBarApp.swift: this file is
//  plumbing only. It answers "where is the CLI" and nothing else — no
//  policy/evidence/action/execution logic, and no judgment about what the
//  CLI should be asked to do.
//
//  Why this type exists at all: GlomerisClient used to default to the
//  single hardcoded path `/usr/local/bin/glomeris`. On Apple Silicon
//  `brew --prefix` is `/opt/homebrew`, so the install route recommended in
//  book/src/installation.md puts the binary at `/opt/homebrew/bin/glomeris`
//  — a path the app never consulted. Following the documented happy path on
//  any Apple Silicon Mac produced a menu-bar app that could not execute
//  anything.
//
//  Why PATH alone is NOT the fix, even though it looks like the obvious
//  one: a GUI app does not inherit a shell's PATH. Measured on macOS 15
//  with a minimal probe .app launched by Finder (the actual user scenario),
//  the process environment was
//    PATH=/usr/bin:/bin:/usr/sbin:/sbin
//  which contains neither `/opt/homebrew/bin` nor `/usr/local/bin`. (The
//  same bundle launched by `open` FROM A SHELL did inherit that shell's
//  PATH, which is exactly why this is easy to get wrong when testing.) So
//  PATH resolution alone would still have left the reported defect in
//  place, and would additionally have broken the Intel case that the old
//  hardcoded path happened to get right. PATH is still consulted — it is
//  what covers a CLI installed somewhere unusual, a developer's local
//  build, and a launch from a terminal — but the known Homebrew prefixes
//  are checked explicitly afterwards rather than being assumed reachable
//  through PATH.
//

import Foundation

/// Where a located `glomeris` binary was found. Carried alongside the URL so
/// precedence is directly assertable in tests, rather than inferrable only
/// from which path happened to come back.
enum GlomerisExecutableSource: Equatable {
    /// Shipped inside the app bundle at `Contents/MacOS/glomeris`, which is
    /// where `.github/workflows/macos-app-release.yml` puts the released CLI
    /// before it ad-hoc signs the bundle.
    case bundled
    /// Found in this directory, taken from the `PATH` environment variable.
    case pathEntry(String)
    /// Found in this known install directory after `PATH` did not resolve it.
    case knownInstallDirectory(String)
}

/// A located binary and where it came from.
struct GlomerisExecutableLocation: Equatable {
    let url: URL
    let source: GlomerisExecutableSource
}

/// Resolves the `glomeris` binary in priority order. Every input is
/// injectable so resolution can be tested without installing anything,
/// without a real app bundle, and without depending on the host's PATH.
struct GlomerisExecutableLocator {
    static let executableName = "glomeris"

    /// Checked after `PATH`, in this order. `/opt/homebrew/bin` is the
    /// Homebrew prefix on Apple Silicon and `/usr/local/bin` on Intel;
    /// `/usr/local/bin` is also where book/src/installation.md tells someone
    /// installing from a release tarball or a source build to copy the
    /// binary, and it is the path the app hardcoded before HORO-1295.
    ///
    /// These are deliberately literal rather than derived from `brew
    /// --prefix`: asking Homebrew would mean locating and spawning `brew`
    /// first, which has the identical problem one level up.
    static let knownInstallDirectories = ["/opt/homebrew/bin", "/usr/local/bin"]

    let bundledExecutableURL: URL?
    let pathVariable: String?
    let knownInstallDirectories: [String]
    let isExecutableFile: (URL) -> Bool

    init(
        bundledExecutableURL: URL? = Bundle.main
            .url(forAuxiliaryExecutable: GlomerisExecutableLocator.executableName),
        pathVariable: String? = ProcessInfo.processInfo.environment["PATH"],
        knownInstallDirectories: [String] = GlomerisExecutableLocator.knownInstallDirectories,
        isExecutableFile: @escaping (URL) -> Bool = {
            FileManager.default.isExecutableFile(atPath: $0.path)
        }
    ) {
        self.bundledExecutableURL = bundledExecutableURL
        self.pathVariable = pathVariable
        self.knownInstallDirectories = knownInstallDirectories
        self.isExecutableFile = isExecutableFile
    }

    /// The first `glomeris` that exists and is executable, or `nil` if none
    /// of the searched locations has one.
    ///
    /// The bundled copy wins because it is version-matched to this app and
    /// covered by the bundle's signature; a mismatched CLI found elsewhere
    /// on the machine could answer with DTO shapes this build cannot decode.
    /// A Debug build embeds no CLI, so development is unaffected by that
    /// precedence.
    func locate() -> GlomerisExecutableLocation? {
        if let bundledExecutableURL, isExecutableFile(bundledExecutableURL) {
            return GlomerisExecutableLocation(url: bundledExecutableURL, source: .bundled)
        }

        for directory in pathDirectories {
            let candidate = Self.candidate(in: directory)
            if isExecutableFile(candidate) {
                return GlomerisExecutableLocation(url: candidate, source: .pathEntry(directory))
            }
        }

        for directory in knownInstallDirectories {
            let candidate = Self.candidate(in: directory)
            if isExecutableFile(candidate) {
                return GlomerisExecutableLocation(
                    url: candidate,
                    source: .knownInstallDirectory(directory)
                )
            }
        }

        return nil
    }

    /// Absolute directories from `PATH`, in order.
    ///
    /// Relative entries — including the empty entry that POSIX shells read as
    /// "the current directory" — are dropped rather than resolved. Honouring
    /// one would make the binary this app executes depend on whatever
    /// directory the process happens to be running in, so anything able to
    /// write a file named `glomeris` into that directory could choose the
    /// executable. The CLI is the component holding all policy and execution
    /// authority, so that is not a substitution to leave available.
    var pathDirectories: [String] {
        guard let pathVariable else { return [] }
        return pathVariable
            .split(separator: ":", omittingEmptySubsequences: false)
            .map(String.init)
            .filter { $0.hasPrefix("/") }
    }

    /// The locations `locate()` searches, named for a user-facing error
    /// message. Derived from the same fields `locate()` reads, so the message
    /// cannot drift from the behaviour it describes.
    var searchedLocations: [String] {
        var locations = ["the app bundle"]
        if !pathDirectories.isEmpty {
            locations.append("PATH")
        }
        locations.append(contentsOf: knownInstallDirectories)
        return locations
    }

    private static func candidate(in directory: String) -> URL {
        URL(fileURLWithPath: directory).appendingPathComponent(executableName)
    }
}
