//! macOS-specific platform adapters: filesystem stat, `launchd` lifecycle,
//! and user notifications. Nothing outside this module should call into
//! `nix`, `osascript`, or `launchctl` directly — always go through the
//! `crate::monitor` traits these types implement.

pub mod statfs;

pub use statfs::MacosFsStat;
