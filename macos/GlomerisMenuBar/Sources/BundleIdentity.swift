//
//  BundleIdentity.swift
//  GlomerisMenuBar
//
//  HORO-1456: the one place this app learns what it is called, so that the
//  names its persistent state is filed under are bound to the running bundle
//  by construction rather than by three string literals that happen to match
//  it.
//
//  Before this, `KeychainCredentialStore.service` and two `UserDefaults`
//  suite names were each the literal `dev.glomeris.GlomerisMenuBar`. That is
//  the release bundle identifier, so nothing shipped misbehaved — but any
//  build carrying a different identifier still read and wrote the release
//  app's state, including the user's stored endpoint, model and BYOK API key.
//  Giving a diagnostic build its own identifier did not isolate it; all three
//  literals had to be patched by hand, which is how HORO-1451's read-back
//  build ended up talking to the configured gateway instead of the local one
//  it was built to talk to.
//
//  Corrects commit 9a04952, which introduced this file claiming the test
//  process has no bundle identifier and therefore takes the fallback branch.
//  Measured: it is `com.apple.dt.xctest.tool`. The fallback is still not the
//  release identifier, for the reason below — but it is a branch nothing the
//  app is normally run as takes, which is why the rule is now also exposed as
//  a function a test can drive.
//

import Foundation

/// The identifier this process files its persistent state under.
///
/// There is one member, and every name the app stores state beneath is derived
/// from it. Two builds with different bundle identifiers therefore cannot see
/// each other's keychain items or preferences, and no source file has to be
/// edited to make that true.
///
/// ## What this resolves to, in each context the app is actually run
///
/// - **The shipped `.app`** — `Sources/Info.plist` sets `CFBundleIdentifier`
///   to `$(PRODUCT_BUNDLE_IDENTIFIER)`, so this is the identifier declared in
///   `project.yml`. Equal, today, to the literal this type replaced: the
///   change is deliberately behaviour-preserving for a released build, which
///   is what makes it safe to make without migrating anyone's stored key.
/// - **A variant build** — a beta channel, a renamed bundle, or a throwaway
///   diagnostic build gets its own identifier and therefore its own state,
///   which is the whole point.
/// - **The unit-test bundle** — `GlomerisMenuBarTests` is a
///   `bundle.unit-test` target with no test host, so `Bundle.main` at test time
///   is Xcode's `xctest` agent and this resolves to `com.apple.dt.xctest.tool`
///   (measured, not assumed). Not the release identifier, which is the point:
///   a test run cannot read or write the state of the app installed on the
///   machine running it.
///
/// ## Why the fallback is not the release identifier
///
/// A fallback to the release identifier would hand the user's real keychain
/// items to whatever process took that branch — reintroducing the defect at the
/// one place a reader would assume it had been fixed. So it is a distinct
/// string, and ``identity(declaredBy:)`` exists so that branch can be asserted
/// rather than reasoned about: nothing the app is normally run as takes it.
///
/// It is not a `fatalError` either. A bundled app always has an identifier, so
/// the crash could never protect a user; it would only turn "loaded outside an
/// app bundle" into "cannot be loaded outside an app bundle".
enum BundleIdentity {
    /// Stands in for the bundle identifier when the running executable has
    /// none — a bare Mach-O tool, for instance. Distinct from every identifier
    /// this project ships, so state filed under it can never be the release
    /// app's state.
    static let unidentifiedProcess = "dev.glomeris.unidentified-process"

    /// The rule, as a function of what the bundle declares, so that both of its
    /// branches can be asserted. `Bundle.main.bundleIdentifier` is whatever the
    /// running process happens to be, and a test cannot make it `nil`.
    static func identity(declaredBy bundleIdentifier: String?) -> String {
        bundleIdentifier ?? unidentifiedProcess
    }

    /// The running bundle's identifier, or ``unidentifiedProcess`` when the
    /// executable is not a bundle that declares one.
    static let current: String = identity(declaredBy: Bundle.main.bundleIdentifier)
}
