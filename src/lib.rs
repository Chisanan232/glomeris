//! Glomeris library crate: disk-pressure monitoring, macOS platform
//! adapters, and the bounded filesystem scanner. Split out from `main.rs`
//! so the pure logic in `monitor`/`scanner` (and their unit tests) doesn't
//! depend on being reachable from a binary's `main`, and so binary wiring
//! in `main.rs` stays a thin CLI shim.

pub mod actionability;
pub mod actions;
pub mod autopilot;
pub mod cli;
pub mod detectors;
pub mod emergency;
pub mod evidence;
pub mod executor;
pub mod monitor;
pub mod platform;
pub mod policy;
pub mod reporting;
pub mod scanner;
