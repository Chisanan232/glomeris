# Installation

## Build from source

Glomeris requires Rust/Cargo (2021 edition). There is no other runtime
dependency.

```sh
git clone https://github.com/Chisanan232/glomeris.git
cd glomeris
cargo build --release
```

The resulting binary is at `target/release/glomeris`. Copy it onto your
`PATH` (e.g. `/usr/local/bin`) if you want to run it as `glomeris` directly.

## Prebuilt releases

There is no prebuilt release binary today. Prebuilt macOS release artifacts
will be published under [GitHub
Releases](https://github.com/Chisanan232/glomeris/releases) once the
packaging/release ticket (HORO-957) lands — check the Releases page directly
for current status.

## Installing the menu-bar app

`GlomerisMenuBar.app.zip` is published as an extra asset on each [GitHub
Release](https://github.com/Chisanan232/glomeris/releases) alongside the CLI
tarball. It's built and ad-hoc signed automatically
(`codesign --force --deep --sign -`, identity `-`) — this seals the bundle
well enough to run, but it is **not** signed with an Apple Developer ID and
**not** notarized. Developer ID notarization is tracked as a future
follow-up, not done today.

That matters the moment you download the zip through a browser: macOS tags
the extracted app with a "quarantine" flag, and Gatekeeper will refuse to
just open it — double-clicking shows **"Apple could not verify that
`GlomerisMenuBar` is free of malware"** with no direct way to proceed. This
is expected for an ad-hoc-signed app and does not mean the download is
broken or unsafe; it's the same warning any non-notarized app gets. Follow
these one-time steps to open it:

1. Unzip `GlomerisMenuBar.app.zip` and move `GlomerisMenuBar.app` wherever
   you want to keep it (e.g. `/Applications`).
2. Double-click it. You'll see the "could not verify" warning — click
   **Done** (or **Cancel**) to dismiss it. This first attempt is expected to
   fail; it's what unlocks the next step.
3. Open **System Settings → Privacy & Security**, scroll down to the
   **Security** section, and you'll see a line like *"GlomerisMenuBar" was
   blocked to protect your Mac* with an **Open Anyway** button next to it.
   Click it, then authenticate with your password/Touch ID when prompted.
4. Double-click `GlomerisMenuBar.app` again. A dialog reappears asking if
   you're sure — click **Open**. macOS remembers this decision, so every
   later launch works with a plain double-click, no repeat of these steps.

This System Settings path is Apple's own documented method for opening
software from an unidentified developer and is the one to use on current
macOS (Sequoia and later, including Tahoe) — recent macOS versions have
locked down the older shortcut of control-clicking the app and choosing
**Open**, so that shortcut may not offer an **Open** option at all anymore.
If it does work for you, it's faster: control-click the app, choose **Open**
from the menu, then click **Open** in the dialog that appears — but treat
the System Settings steps above as the reliable path.

**Terminal fallback (last resort).** If neither of the above surfaces an
**Open Anyway**/**Open** option — for example the security exception was
already dismissed, or you're scripting the install — remove the quarantine
flag directly:

```sh
xattr -d com.apple.quarantine /path/to/GlomerisMenuBar.app
```

Only run this against an app you actually intend to trust: it deletes the
flag that tells Gatekeeper to check the app at all, so use it deliberately,
not as a routine habit.

## Platform support

Glomeris targets macOS only. `daemon`, `emergency`, and `free` are gated on
`#[cfg(target_os = "macos")]` and print an error and exit non-zero on any
other OS; `scan` and `detect` do not have this restriction, since they only
touch `std::fs`/`std::process` and don't depend on the macOS-only
`platform::macos` module.

## Verifying the build

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Both are part of this repository's CI (`.github/workflows/ci.yml`, macOS
runner) and should pass on a clean checkout.
