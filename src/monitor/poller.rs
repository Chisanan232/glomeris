//! Bounded-interval polling loop.
//!
//! Wires together [`FsStat`] (I/O), [`ThresholdConfig`]/[`PressureStateMachine`]
//! (pure logic), [`Notifier`] and [`PersistenceBackend`] (I/O), and a
//! [`Clock`] so the whole loop is deterministic and unit-testable without
//! touching a real disk, sending a real notification, or sleeping real
//! time.
//!
//! The healthy path here calls [`FsStat::stat`] and nothing else — it never
//! reaches into the scanner or walks a directory tree.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::monitor::clock::Clock;
use crate::monitor::config::ThresholdConfig;
use crate::monitor::fs_stat::FsStat;
use crate::monitor::notifier::Notifier;
use crate::monitor::persistence::{unix_now_secs, PersistenceBackend, PressureEvent};
use crate::monitor::pressure::PressureState;
use crate::monitor::state_machine::PressureStateMachine;

/// Poller configuration: which path to watch and how often.
#[derive(Debug, Clone)]
pub struct PollConfig {
    pub watch_path: PathBuf,
    pub poll_interval: Duration,
    /// Consecutive matching observations required before a state
    /// transition is confirmed (see `PressureStateMachine`).
    pub confirm_after: u32,
}

impl PollConfig {
    pub fn new(watch_path: impl Into<PathBuf>) -> Self {
        Self {
            watch_path: watch_path.into(),
            poll_interval: default_poll_interval(),
            confirm_after: default_confirm_after(),
        }
    }
}

/// Default polling cadence: frequent enough to catch a fast-filling disk,
/// cheap enough (one `statfs` call) to never matter for idle resource use.
pub fn default_poll_interval() -> Duration {
    Duration::from_secs(60)
}

/// Default debounce window: two consecutive polls (two minutes at the
/// default interval) before a boundary crossing is confirmed.
pub fn default_confirm_after() -> u32 {
    2
}

/// Outcome of a single poll iteration, returned so callers/tests can assert
/// on what happened without depending on log output.
#[derive(Debug, Clone)]
pub struct PollOutcome {
    pub observed_state: PressureState,
    pub transitioned: bool,
    pub notify_error: Option<String>,
    pub persist_error: Option<String>,
}

/// Runs one observe → classify → (maybe) notify/persist step. Never panics
/// on a notification or persistence failure — both are reported in the
/// returned [`PollOutcome`] and otherwise swallowed so the caller's loop
/// keeps running.
#[allow(clippy::too_many_arguments)]
pub fn poll_once(
    watch_path: &Path,
    thresholds: &ThresholdConfig,
    machine: &mut PressureStateMachine,
    fs_stat: &dyn FsStat,
    notifier: &dyn Notifier,
    persistence: &dyn PersistenceBackend,
) -> io::Result<PollOutcome> {
    let usage = fs_stat.stat(watch_path)?;
    let used_percent = usage.used_percent();
    let observed_state = thresholds.classify(used_percent, usage.free_bytes);

    let mut outcome = PollOutcome {
        observed_state,
        transitioned: false,
        notify_error: None,
        persist_error: None,
    };

    if let Some(transition) = machine.observe(observed_state) {
        outcome.transitioned = true;

        if let Err(e) = notifier.notify(transition.from, transition.to) {
            outcome.notify_error = Some(e.to_string());
        }

        let event = PressureEvent {
            unix_time_secs: unix_now_secs(),
            from: transition.from,
            to: transition.to,
            used_percent,
            free_bytes: usage.free_bytes,
        };
        if let Err(e) = persistence.record(&event) {
            outcome.persist_error = Some(e.to_string());
        }
    }

    Ok(outcome)
}

/// Runs the polling loop, sleeping `poll_interval` between iterations via
/// `clock`. `max_iterations` bounds the loop (used by tests); production
/// callers pass `None` to run until the process is terminated.
///
/// A single failed `FsStat::stat` call (e.g. a transient statfs error) is
/// logged to `on_iteration` as an outcome-less iteration and does not stop
/// the loop — only an unrecoverable setup error would do that, and there is
/// none in this loop's steady state.
#[allow(clippy::too_many_arguments)]
pub fn run<C, F, N, P>(
    config: &PollConfig,
    thresholds: &ThresholdConfig,
    clock: &C,
    fs_stat: &F,
    notifier: &N,
    persistence: &P,
    max_iterations: Option<u64>,
    mut on_iteration: impl FnMut(io::Result<PollOutcome>),
) where
    C: Clock,
    F: FsStat,
    N: Notifier,
    P: PersistenceBackend,
{
    let mut machine = PressureStateMachine::new(PressureState::Healthy, config.confirm_after);
    let mut iterations: u64 = 0;

    loop {
        if let Some(max) = max_iterations {
            if iterations >= max {
                break;
            }
        }

        let outcome = poll_once(
            &config.watch_path,
            thresholds,
            &mut machine,
            fs_stat,
            notifier,
            persistence,
        );
        on_iteration(outcome);

        iterations += 1;
        if let Some(max) = max_iterations {
            if iterations >= max {
                break;
            }
        }
        clock.sleep(config.poll_interval);
    }
}
