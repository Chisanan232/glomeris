//
//  GlomerisCliIdentity.swift
//  GlomerisMenuBar
//
//  HORO-1466: which `glomeris` this app is actually driving, and whether it
//  is the one the app was built against.
//
//  See the standing project rule in GlomerisMenuBarApp.swift: this file is
//  plumbing only. It answers "which binary, and is it the expected one" and
//  nothing else — no policy/evidence/action/execution logic, and in
//  particular no decision about whether a mismatched CLI should be used. That
//  is deliberately out of scope for this ticket: the mismatch has to become
//  visible before anyone can sensibly decide what to do about it.
//
//  ============================================================================
//  THE DEFECT THIS EXISTS FOR
//  ============================================================================
//  `GlomerisExecutableLocator.locate()` has always known the path and the
//  source. Both were surfaced in exactly one place — the
//  `executableNotFound` error, which by construction only renders when
//  NOTHING was found. On the success path both were discarded, so an app
//  driving a CLI that predates the current fail-closed executor work was
//  indistinguishable, from inside the product, from one driving a matching
//  CLI.
//
//  That is not hypothetical. Two binaries measured on one machine both
//  answered `--version` with `0.2.0` while disagreeing about whether an
//  unscoped mutating action may be offered at all: one offered a `brew`
//  cleanup action with no `scoped_path`, the other refused it. Dogfood
//  observations taken from the first describe behaviour from before HORO-1322,
//  HORO-1327, HORO-1358, HORO-1359 and HORO-1360 merged, and nothing in the
//  product said so.
//
//  ============================================================================
//  WHY THE VERSION STRING CANNOT BE THE IDENTITY
//  ============================================================================
//  It is stamped from the crate version, so it does not change between
//  merges. The two binaries above are the proof: same version string,
//  opposite safety posture. A version comparison here would be a check that
//  cannot fail for the case it exists to catch, which is worse than no check
//  — it would read as coverage.
//
//  So the identity is a hash of the bytes. `.github/workflows/
//  macos-app-release.yml` records the SHA-256 of the CLI it embeds into the
//  bundle's Info.plist under ``GlomerisCliIdentity/expectedHashInfoKey``, and
//  this file hashes whatever the locator resolved and compares the two.
//
//  ============================================================================
//  WHY "NO EXPECTATION" IS A STATE AND NOT A FAILURE
//  ============================================================================
//  A locally built app embeds no CLI at all: the embed step lives in the
//  release workflow, not in the Xcode project, so `xcodebuild` output always
//  falls through to PATH or the known install directories. Such a build also
//  stamps no expected hash, and it must say *that* rather than claim a
//  mismatch — reporting "not the build this app ships" to a developer whose
//  app ships nothing would be a false alarm, and false alarms are how a real
//  one gets ignored. ``GlomerisCliExpectation/notDeclared`` is that state.
//

import CryptoKit
import Foundation

/// The bytes of a located `glomeris`, or why they could not be read.
///
/// Deliberately an enum rather than a `String?`. An optional would let a
/// caller render the resolved path while silently dropping the identity —
/// which is the exact shape of the defect this ticket is about, one level up.
/// With two cases there is no "absent" to forget: every code path that has a
/// path also has something to say about the bytes at it.
enum GlomerisCliContent: Equatable {
    /// Lowercase hex SHA-256 of the whole file.
    case sha256(String)
    /// The file is executable — the locator only returns executable files —
    /// but could not be read. Mode `111` does this: the kernel will exec it
    /// and `read(2)` will not. The associated text is one clause fit for the
    /// end of a sentence.
    case unreadable(String)

    /// The first 12 hex characters, for a 260pt popover row. 48 bits is far
    /// more than enough to tell two builds of one program apart; the whole
    /// hash stays available in the tooltip and the accessibility label, so
    /// nothing is hidden, only abbreviated.
    var shortDescription: String {
        switch self {
        case .sha256(let hash):
            return String(hash.prefix(12))
        case .unreadable:
            return "unreadable"
        }
    }
}

/// Whether the resolved CLI is the one this app was built against.
enum GlomerisCliExpectation: Equatable {
    /// The resolved binary hashes to the hash this app records.
    case matches
    /// Both hashes are known and they differ. Carries the expected one so the
    /// UI can show what it was looking for alongside what it found.
    case differs(expected: String)
    /// This app records no expected hash — a locally built app, which embeds
    /// no CLI. See the file header.
    case notDeclared
    /// This app records an expected hash but the resolved binary's bytes could
    /// not be read, so the comparison could not be made either way. Distinct
    /// from ``notDeclared``, which is about this app, not about the binary.
    case notComparable
}

/// A resolved `glomeris`, its content identity, and whether that identity is
/// the expected one.
struct GlomerisCliIdentity: Equatable {
    /// The Info.plist key the release workflow stamps the embedded CLI's
    /// SHA-256 into. Declared here and read from here, so the workflow and
    /// the app cannot drift apart silently — `scripts/
    /// check-cli-identity-is-reported.sh` compares the two.
    static let expectedHashInfoKey = "GlomerisExpectedCLISHA256"

    let location: GlomerisExecutableLocation
    let content: GlomerisCliContent
    let expectation: GlomerisCliExpectation

    /// `expectation` is derived here rather than passed in, so it cannot
    /// contradict `content`. A caller cannot construct an identity that
    /// reports `matches` next to unreadable bytes.
    ///
    /// `declaredExpectedHash` is compared case-insensitively after trimming,
    /// and a blank value counts as absent: a build setting that failed to
    /// expand leaves an empty string, and an empty string must not read as
    /// "expected nothing, therefore mismatch".
    init(
        location: GlomerisExecutableLocation,
        content: GlomerisCliContent,
        declaredExpectedHash: String?
    ) {
        self.location = location
        self.content = content
        self.expectation = Self.expectation(
            for: content,
            declaredExpectedHash: declaredExpectedHash
        )
    }

    /// The comparison rule, as a function of its two inputs, so both of its
    /// branches can be asserted without building a file to hash.
    static func expectation(
        for content: GlomerisCliContent,
        declaredExpectedHash: String?
    ) -> GlomerisCliExpectation {
        let declared = declaredExpectedHash?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()

        guard let declared, !declared.isEmpty else {
            return .notDeclared
        }
        switch content {
        case .unreadable:
            return .notComparable
        case .sha256(let hash):
            return hash.lowercased() == declared ? .matches : .differs(expected: declared)
        }
    }

    /// Lowercase hex SHA-256 of the file at `url`, read in 1 MiB chunks.
    ///
    /// Chunked rather than `Data(contentsOf:)` because this runs against a
    /// binary of unbounded size that the app does not control — the resolved
    /// CLI can be any file on the machine named `glomeris` — and mapping an
    /// arbitrary file wholesale into a menu-bar app's address space to hash it
    /// is a cost with no upside.
    static func sha256OfFile(_ url: URL) -> GlomerisCliContent {
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            return .unreadable("its contents could not be read")
        }
        defer { try? handle.close() }

        var hasher = SHA256()
        while true {
            let chunk: Data?
            do {
                chunk = try handle.read(upToCount: 1 << 20)
            } catch {
                return .unreadable("reading it stopped partway through")
            }
            guard let chunk, !chunk.isEmpty else { break }
            hasher.update(data: chunk)
        }
        return .sha256(hasher.finalize().map { String(format: "%02x", $0) }.joined())
    }
}

/// What a probe found. There is no case carrying a location without content,
/// which is what makes "reported a path and no identity" unrepresentable
/// rather than merely discouraged.
enum GlomerisCliIdentityOutcome: Equatable {
    /// Nothing named `glomeris` was executable in any searched location. The
    /// names are the same ones `GlomerisClientError.executableNotFound`
    /// carries, so one install problem reads the same in both places.
    case notFound(searched: [String])
    case identified(GlomerisCliIdentity)
}

/// Resolves the CLI and identifies it. Every input is injectable so the whole
/// rule can be tested without installing a binary, without a real app bundle,
/// and without hashing anything.
struct GlomerisCliIdentityProbe {
    let locator: GlomerisExecutableLocator
    /// The hash this app was built against, or `nil`/blank when it records
    /// none. Read from the bundle by default.
    let declaredExpectedHash: String?
    let contentOf: (URL) -> GlomerisCliContent

    init(
        locator: GlomerisExecutableLocator = GlomerisExecutableLocator(),
        declaredExpectedHash: String? = Bundle.main
            .object(forInfoDictionaryKey: GlomerisCliIdentity.expectedHashInfoKey) as? String,
        contentOf: @escaping (URL) -> GlomerisCliContent = GlomerisCliIdentity.sha256OfFile
    ) {
        self.locator = locator
        self.declaredExpectedHash = declaredExpectedHash
        self.contentOf = contentOf
    }

    /// Resolves and identifies, hashing on the calling thread.
    ///
    /// Not cached. Hashing the binary costs one page-cached read of a few
    /// megabytes and happens only while the popover is open, off the main
    /// actor; a cache keyed on size and modification time would buy that back
    /// at the price of a staleness bug in the one surface whose job is to say
    /// honestly which binary is in use. Re-resolving every poll is also the
    /// behaviour `GlomerisClient` already has, and for the same reason:
    /// installing or replacing the CLI while the app runs takes effect at the
    /// next poll rather than at the next restart.
    func probe() -> GlomerisCliIdentityOutcome {
        guard let location = locator.locate() else {
            return .notFound(searched: locator.searchedLocations)
        }
        return .identified(
            GlomerisCliIdentity(
                location: location,
                content: contentOf(location.url),
                declaredExpectedHash: declaredExpectedHash
            )
        )
    }

    /// ``probe()`` off the main thread, so hashing a multi-megabyte binary
    /// cannot stutter the popover it is being rendered into.
    ///
    /// Same shape as `GlomerisClient.readAll`: a continuation resumed from a
    /// global queue. The section this feeds is `@MainActor`, and a synchronous
    /// call from there would do the read on the main thread.
    func probeOffMainThread() async -> GlomerisCliIdentityOutcome {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                continuation.resume(returning: probe())
            }
        }
    }
}
