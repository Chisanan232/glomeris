//! Platform-specific adapters, isolated behind the `crate::monitor` traits.

#[cfg(target_os = "macos")]
pub mod macos;
