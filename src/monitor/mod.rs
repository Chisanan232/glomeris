//! Disk-pressure monitoring: pure state logic plus the polling loop that
//! drives it. Everything platform-specific (statfs, notifications, launchd)
//! lives under `crate::platform::macos` and is wired in through the traits
//! defined here (`FsStat`, `Notifier`, `PersistenceBackend`, `Clock`).

pub mod clock;
pub mod config;
pub mod fs_stat;
pub mod notifier;
pub mod pressure;
pub mod state_machine;

pub use clock::{Clock, FakeClock, SystemClock};
pub use config::ThresholdConfig;
pub use fs_stat::{FsStat, FsUsage};
pub use notifier::Notifier;
pub use pressure::PressureState;
pub use state_machine::{PressureStateMachine, Transition};
