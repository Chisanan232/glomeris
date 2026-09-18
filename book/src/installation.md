# Installation

## Homebrew tap (recommended)

Every tagged release is built and published automatically by
[`cargo-dist`](https://github.com/axodotdev/cargo-dist)
(`.github/workflows/release.yml`), which also generates and pushes a
Homebrew formula to the
[`Chisanan232/homebrew-tap`](https://github.com/Chisanan232/homebrew-tap)
repository (HORO-1069). This is the simplest way to install and update the
`glomeris` CLI:

```sh
brew tap chisanan232/tap
brew install glomeris
```

`brew upgrade glomeris` picks up new releases the same way. The formula
installs the `glomeris` binary onto your `PATH` — no separate `cargo
build` step needed.

Two macOS targets are built for every release: `aarch64-apple-darwin`
(Apple Silicon) and `x86_64-apple-darwin` (Intel, cross-compiled) — see
`dist-workspace.toml`. Homebrew selects the right one automatically.

## Prebuilt release archives

If you'd rather not use Homebrew, the same `aarch64-apple-darwin`/
`x86_64-apple-darwin` tarballs `cargo-dist` builds for the formula above
are also attached directly to each [GitHub
Release](https://github.com/Chisanan232/glomeris/releases) — download the
one matching your Mac's architecture, extract it, and copy the `glomeris`
binary onto your `PATH` (e.g. `/usr/local/bin`).

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

## Installing the menu-bar app

See [Menu Bar App](menu_bar_app.md) for what `GlomerisMenuBar.app` actually
shows and lets you do once it's installed.

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
