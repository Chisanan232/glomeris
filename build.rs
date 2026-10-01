//! Embeds a dev/release distinction into `--version` (HORO-1612/HORO-1609).
//!
//! `cargo-dist` builds the release artifact from a tagged commit (`v<version>`
//! matching `Cargo.toml`'s `version`), so if the working tree at build time is
//! exactly that tag with no extra commits and nothing dirty, this is a real
//! release build. Anything else -- extra commits, a dirty tree, no matching
//! tag, or no `.git` at all (e.g. a source tarball with history stripped) --
//! cannot make that claim, so it must not print the bare release-looking
//! version string.
//!
//! Runs only at build time; the runtime binary never shells out to `git`
//! (HORO-1609 AC4: `--version` must work offline).

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let expected_tag = format!("v{pkg_version}");

    let describe = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let build_rev = match describe {
        Some(ref d) if d == &expected_tag => "release".to_string(),
        Some(d) => d,
        None => "unknown".to_string(),
    };

    println!("cargo:rustc-env=GLOMERIS_BUILD_REV={build_rev}");
}
