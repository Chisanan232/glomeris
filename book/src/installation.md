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
