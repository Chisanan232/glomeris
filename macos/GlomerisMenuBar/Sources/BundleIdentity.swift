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
///   `bundle.unit-test` target with no test host, so `Bundle.main` at test
///   time is the `xctest` tool. Its embedded `Info.plist` declares no
///   `CFBundleIdentifier`, so `bundleIdentifier` is `nil` and the fallback
///   below is what every test run uses.
///
/// ## Why the fallback is not the release identifier
///
/// Because that branch is not hypothetical — it is the branch taken by every
/// test run, and by anything else loading this code outside an app bundle. A
/// fallback to the release identifier would hand exactly those processes the
/// user's real keychain items, reintroducing the defect at the one place a
/// reader would assume it had been fixed.
///
/// It is not a `fatalError` either. A bundled app always has an identifier, so
/// the crash would never protect a user; it would only turn "runs in a test
/// process" into "cannot run in a test process".
enum BundleIdentity {
    /// Stands in for the bundle identifier when the running executable has
    /// none. Distinct from every identifier this project ships, so state filed
    /// under it can never be the release app's state.
    static let unidentifiedProcess = "dev.glomeris.unidentified-process"

    /// The running bundle's identifier, or ``unidentifiedProcess`` when the
    /// executable is not a bundle that declares one.
    static let current: String = Bundle.main.bundleIdentifier ?? unidentifiedProcess
}
