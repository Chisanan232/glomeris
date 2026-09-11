//! macOS-specific platform adapters: filesystem stat, `launchd` lifecycle,
//! and user notifications. Nothing outside this module should call into
//! `nix`, `osascript`, or `launchctl` directly — always go through the
//! `crate::monitor` traits these types implement.

pub mod launchd;
pub mod notify;
pub mod statfs;

pub use notify::MacosNotifier;
pub use statfs::MacosFsStat;
