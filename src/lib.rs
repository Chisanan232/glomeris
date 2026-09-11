//! Glomeris library crate: disk-pressure monitoring, macOS platform
//! adapters, and the bounded filesystem scanner. Split out from `main.rs`
//! so the pure logic in `monitor`/`scanner` (and their unit tests) doesn't
//! depend on being reachable from a binary's `main`, and so binary wiring
//! in `main.rs` stays a thin CLI shim.

pub mod monitor;
pub mod platform;
pub mod scanner;
