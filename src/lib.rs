//! Glomeris library crate: disk-pressure monitoring and macOS platform
//! adapters. Split out from `main.rs` so the pure logic in `monitor` (and
//! its unit tests) doesn't depend on being reachable from a binary's
//! `main`, and so binary wiring in `main.rs` stays a thin CLI shim.

pub mod monitor;
