# Installation

## Prebuilt release archives (recommended)

Every tagged release is built and published automatically by
[`cargo-dist`](https://github.com/axodotdev/cargo-dist)
(`.github/workflows/release.yml`), which attaches the binary tarballs
directly to each [GitHub
Release](https://github.com/Chisanan232/glomeris/releases) with checksums.
Download the one matching your Mac's architecture, extract it, and copy the
`glomeris` binary onto your `PATH` (e.g. `/usr/local/bin`).

Two macOS targets are built for every release: `aarch64-apple-darwin`
(Apple Silicon) and `x86_64-apple-darwin` (Intel, cross-compiled) — see
`dist-workspace.toml`. Pick by architecture; nothing selects it for you.

## Homebrew tap

```sh
brew tap Chisanan232/tap
brew trust --tap chisanan232/tap   # Homebrew 7 and later only; see below
brew install glomeris
```

`brew upgrade glomeris` picks up later releases the same way, and Homebrew
selects the right architecture automatically.

The tap is [`Chisanan232/homebrew-tap`](https://github.com/Chisanan232/homebrew-tap),
and `dist-workspace.toml` points `cargo-dist` at it so each release can publish
a regenerated formula there (HORO-1069, HORO-1320).

Three things about this path are worth knowing before you use it.

**Homebrew 7 will not load a third-party tap until you trust it.** On Homebrew
7.0 and later, `brew install glomeris` straight after `brew tap` fails with
`Refusing to load formula chisanan232/tap/glomeris from untrusted tap`. That is
Homebrew protecting you from arbitrary Ruby in a tap you have not vouched for,
not a broken formula. `brew trust --tap chisanan232/tap` records the decision in
`~/.homebrew/trust.json` (or under `$XDG_CONFIG_HOME/homebrew/`) and only needs
doing once. Older Homebrew versions have no `brew trust` and do not need this
step.

**A manually copied binary already on your `PATH` blocks the symlink.** If you
previously followed the prebuilt-archive instructions above and copied
`glomeris` into `/opt/homebrew/bin` or `/usr/local/bin`, Homebrew installs into
its Cellar but refuses to link over your file, and warns that the Homebrew copy
is shadowed. Remove your manual copy, or let Homebrew take ownership:

```sh
brew link --overwrite glomeris --dry-run   # lists exactly what it would remove
brew link --overwrite glomeris
```

**The formula currently tracks `v0.2.0` and is updated by hand.** The release
workflow's `publish-homebrew-formula` job needs a credential for the tap
repository that does not exist yet, so no release has regenerated the formula
automatically. That credential is the remaining half of HORO-1320. Until it is
configured, the tap can lag the newest tagged release — check the
[releases page](https://github.com/Chisanan232/glomeris/releases) if you need
the very latest, or use a prebuilt archive above.

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

The app runs the `glomeris` CLI for everything it displays. A release build
ships its own copy inside the bundle, so it works with no separate CLI
install; otherwise it looks along `PATH` and then in `/opt/homebrew/bin` and
`/usr/local/bin`, which covers both Homebrew prefixes and the manual-copy
instructions above. [Menu Bar
App](menu_bar_app.md#how-the-app-finds-the-glomeris-cli) documents the exact
order and why `PATH` alone is not enough for a GUI app.

From the next tagged release onward, `GlomerisMenuBar.app.zip` is published
as an extra asset on that [GitHub
Release](https://github.com/Chisanan232/glomeris/releases) alongside the CLI
tarball, by a separate `macos-app-release.yml` workflow that fires once the
release is published. That workflow landed after the current latest release
was tagged, so releases published so far carry only the CLI tarballs — to
get the app before the next tag, build
`macos/GlomerisMenuBar/GlomerisMenuBar.xcodeproj` yourself. It's built and
ad-hoc signed automatically
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
