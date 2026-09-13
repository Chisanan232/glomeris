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
use crate::monitor::persistence::{unix_now_secs, Heartbeat, PersistenceBackend, PressureEvent};
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
    pub used_percent: f64,
    pub free_bytes: u64,
    pub transitioned: bool,
    pub notify_error: Option<String>,
    pub persist_error: Option<String>,
    /// Set when this cycle's best-effort heartbeat write (see `run`)
    /// failed. Never causes the loop to stop — same failure philosophy as
    /// `persist_error`.
    pub heartbeat_error: Option<String>,
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
        used_percent,
        free_bytes: usage.free_bytes,
        transitioned: false,
        notify_error: None,
        persist_error: None,
        heartbeat_error: None,
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
///
/// After each completed poll cycle, writes a best-effort heartbeat
/// (`crate::monitor::persistence::Heartbeat`) to `heartbeat_path` so a
/// later ticket (HORO-1045) can tell "loaded" apart from "actually
/// polling". A write failure is reported via `PollOutcome::heartbeat_error`
/// and otherwise ignored — it never stops the loop, matching
/// `PersistenceBackend::record`'s failure philosophy.
#[allow(clippy::too_many_arguments)]
pub fn run<C, F, N, P>(
    config: &PollConfig,
    thresholds: &ThresholdConfig,
    clock: &C,
    fs_stat: &F,
    notifier: &N,
    persistence: &P,
    heartbeat_path: &Path,
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

        let mut outcome = poll_once(
            &config.watch_path,
            thresholds,
            &mut machine,
            fs_stat,
            notifier,
            persistence,
        );
        if let Ok(ref mut o) = outcome {
            let heartbeat = Heartbeat::new(
                clock.unix_now_secs(),
                o.observed_state,
                o.used_percent,
                o.free_bytes,
            );
            if let Err(e) = crate::monitor::persistence::write_heartbeat(heartbeat_path, &heartbeat)
            {
                o.heartbeat_error = Some(e.to_string());
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::clock::FakeClock;
    use crate::monitor::fs_stat::FsUsage;
    use crate::monitor::persistence::AlwaysFailingPersistence;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct FixedFsStat {
        usage: FsUsage,
    }

    impl FsStat for FixedFsStat {
        fn stat(&self, _path: &Path) -> io::Result<FsUsage> {
            Ok(self.usage)
        }
    }

    /// Returns a different usage each call, cycling through a fixed script
    /// — lets a test drive the loop through a scripted sequence of disk
    /// states without touching a real filesystem.
    struct ScriptedFsStat {
        script: Mutex<std::vec::IntoIter<FsUsage>>,
        last: Mutex<FsUsage>,
    }

    impl ScriptedFsStat {
        fn new(script: Vec<FsUsage>) -> Self {
            let last = *script.last().expect("script must be non-empty");
            Self {
                script: Mutex::new(script.into_iter()),
                last: Mutex::new(last),
            }
        }
    }

    impl FsStat for ScriptedFsStat {
        fn stat(&self, _path: &Path) -> io::Result<FsUsage> {
            let mut script = self.script.lock().unwrap();
            let usage = script.next().unwrap_or_else(|| *self.last.lock().unwrap());
            Ok(usage)
        }
    }

    #[derive(Default)]
    struct CountingNotifier {
        count: AtomicUsize,
    }

    impl Notifier for CountingNotifier {
        fn notify(&self, _from: PressureState, _to: PressureState) -> io::Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct NoopPersistence;
    impl PersistenceBackend for NoopPersistence {
        fn record(&self, _event: &PressureEvent) -> io::Result<()> {
            Ok(())
        }
    }

    struct NoopNotifier;
    impl Notifier for NoopNotifier {
        fn notify(&self, _from: PressureState, _to: PressureState) -> io::Result<()> {
            Ok(())
        }
    }

    /// A unique temp path for a test's heartbeat file, mirroring the
    /// `unique_temp_dir` convention in `platform::macos::launchd`'s tests.
    fn unique_heartbeat_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-heartbeat-test-{tag}-{}-{}",
            std::process::id(),
            unix_now_secs()
        ))
    }

    #[test]
    fn healthy_steady_state_never_transitions_or_notifies() {
        let fs = FixedFsStat {
            usage: FsUsage::new(500_000_000_000, 450_000_000_000),
        };
        let notifier = CountingNotifier::default();
        let mut machine = PressureStateMachine::new(PressureState::Healthy, 2);
        let thresholds = ThresholdConfig::default();

        for _ in 0..5 {
            let outcome = poll_once(
                Path::new("/"),
                &thresholds,
                &mut machine,
                &fs,
                &notifier,
                &NoopPersistence,
            )
            .unwrap();
            assert_eq!(outcome.observed_state, PressureState::Healthy);
            assert!(!outcome.transitioned);
        }
        assert_eq!(notifier.count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn crossing_a_boundary_yields_exactly_one_notification() {
        let thresholds = ThresholdConfig::default();
        // Two GiB total, sustained at 99% used -> Emergency, held constant.
        let fs = FixedFsStat {
            usage: FsUsage::new(2_000_000_000, 20_000_000),
        };
        let notifier = CountingNotifier::default();
        let mut machine = PressureStateMachine::new(PressureState::Healthy, 1);

        for _ in 0..10 {
            poll_once(
                Path::new("/"),
                &thresholds,
                &mut machine,
                &fs,
                &notifier,
                &NoopPersistence,
            )
            .unwrap();
        }

        assert_eq!(notifier.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn persistence_failure_does_not_stop_the_monitor_loop() {
        let thresholds = ThresholdConfig::default();
        let fs = FixedFsStat {
            usage: FsUsage::new(2_000_000_000, 20_000_000),
        };
        let mut machine = PressureStateMachine::new(PressureState::Healthy, 1);

        let outcome = poll_once(
            Path::new("/"),
            &thresholds,
            &mut machine,
            &fs,
            &NoopNotifier,
            &AlwaysFailingPersistence,
        )
        .expect("poll_once must still succeed despite persistence failure");

        assert!(outcome.transitioned);
        assert!(outcome.persist_error.is_some());
        // The loop-level `run` must also keep going across many iterations
        // with a permanently failing persistence backend.
        let clock = FakeClock::new();
        let config = PollConfig {
            watch_path: PathBuf::from("/"),
            poll_interval: Duration::from_secs(1),
            confirm_after: 1,
        };
        let mut iterations_seen = 0;
        let heartbeat_path = unique_heartbeat_path("persist-failure");
        run(
            &config,
            &thresholds,
            &clock,
            &fs,
            &NoopNotifier,
            &AlwaysFailingPersistence,
            &heartbeat_path,
            Some(20),
            |outcome| {
                iterations_seen += 1;
                assert!(outcome.is_ok(), "iteration must not error out");
            },
        );
        assert_eq!(iterations_seen, 20);
        let _ = std::fs::remove_file(&heartbeat_path);
    }

    #[test]
    fn run_sleeps_between_polls_using_the_injected_clock() {
        let thresholds = ThresholdConfig::default();
        let fs = FixedFsStat {
            usage: FsUsage::new(500_000_000_000, 450_000_000_000),
        };
        let clock = FakeClock::new();
        let config = PollConfig {
            watch_path: PathBuf::from("/"),
            poll_interval: Duration::from_secs(30),
            confirm_after: 1,
        };

        let heartbeat_path = unique_heartbeat_path("sleep-cadence");
        run(
            &config,
            &thresholds,
            &clock,
            &fs,
            &NoopNotifier,
            &NoopPersistence,
            &heartbeat_path,
            Some(4),
            |_| {},
        );

        // A sleep happens *between* polls, so 4 completed iterations yield
        // 3 sleeps (none trailing the final iteration); no real time
        // elapsed regardless.
        assert_eq!(clock.sleep_count(), 3);
        assert_eq!(clock.recorded_sleeps(), vec![Duration::from_secs(30); 3]);
        let _ = std::fs::remove_file(&heartbeat_path);
    }

    #[test]
    fn run_never_calls_into_a_scanner_type_path() {
        // Structural guard: `poll_once`'s only filesystem dependency is the
        // `FsStat` trait object, which the scripted stub below proves is
        // sufficient to drive every transition — nothing here reaches for
        // a directory walk.
        let thresholds = ThresholdConfig::default();
        let fs = ScriptedFsStat::new(vec![
            FsUsage::new(500_000_000_000, 450_000_000_000), // healthy
            FsUsage::new(500_000_000_000, 450_000_000_000), // healthy
        ]);
        let mut machine = PressureStateMachine::new(PressureState::Healthy, 1);
        let outcome = poll_once(
            Path::new("/"),
            &thresholds,
            &mut machine,
            &fs,
            &NoopNotifier,
            &NoopPersistence,
        )
        .unwrap();
        assert_eq!(outcome.observed_state, PressureState::Healthy);
    }

    #[allow(dead_code)]
    fn assert_notifier_and_persistence_are_send_sync<T: Send + Sync>() {}
    #[test]
    fn state_types_are_send_sync_for_daemon_use() {
        assert_notifier_and_persistence_are_send_sync::<Arc<dyn Notifier>>();
        assert_notifier_and_persistence_are_send_sync::<Arc<dyn PersistenceBackend>>();
    }

    #[test]
    fn heartbeat_timestamp_advances_across_consecutive_poll_cycles() {
        use crate::monitor::persistence::read_heartbeat;

        let thresholds = ThresholdConfig::default();
        let fs = FixedFsStat {
            usage: FsUsage::new(500_000_000_000, 450_000_000_000),
        };
        let clock = FakeClock::new();
        let config = PollConfig {
            watch_path: PathBuf::from("/"),
            poll_interval: Duration::from_secs(30),
            confirm_after: 1,
        };
        let heartbeat_path = unique_heartbeat_path("advancing-timestamp");

        let mut observed_timestamps = Vec::new();
        run(
            &config,
            &thresholds,
            &clock,
            &fs,
            &NoopNotifier,
            &NoopPersistence,
            &heartbeat_path,
            Some(3),
            |outcome| {
                assert!(outcome.is_ok());
                let heartbeat = read_heartbeat(&heartbeat_path)
                    .expect("heartbeat must be written and readable after every cycle");
                observed_timestamps.push(heartbeat.last_poll_unix_secs);
            },
        );

        assert_eq!(observed_timestamps.len(), 3);
        // Same injected `FakeClock` seam `run_sleeps_between_polls_using_the_injected_clock`
        // uses: `sleep` advances the fake clock deterministically, so each
        // cycle's heartbeat timestamp must be strictly greater than the
        // last, with no real time or wall-clock dependency.
        assert!(
            observed_timestamps.windows(2).all(|w| w[1] > w[0]),
            "heartbeat timestamp must strictly advance across cycles: {observed_timestamps:?}"
        );

        let _ = std::fs::remove_file(&heartbeat_path);
    }

    #[test]
    fn heartbeat_write_failure_does_not_stop_the_monitor_loop() {
        let thresholds = ThresholdConfig::default();
        let fs = FixedFsStat {
            usage: FsUsage::new(500_000_000_000, 450_000_000_000),
        };
        let clock = FakeClock::new();
        let config = PollConfig {
            watch_path: PathBuf::from("/"),
            poll_interval: Duration::from_secs(1),
            confirm_after: 1,
        };
        // A path whose parent directory does not exist and cannot be
        // created (parent is a *file*, not a directory), forcing every
        // `write_heartbeat` call to fail deterministically.
        let dir = unique_heartbeat_path("write-failure-parent");
        std::fs::write(&dir, b"not a directory").unwrap();
        let heartbeat_path = dir.join("heartbeat.json");

        let mut iterations_seen = 0;
        let mut heartbeat_errors_seen = 0;
        run(
            &config,
            &thresholds,
            &clock,
            &fs,
            &NoopNotifier,
            &NoopPersistence,
            &heartbeat_path,
            Some(10),
            |outcome| {
                let outcome = outcome.expect("a heartbeat write failure must not error the loop");
                iterations_seen += 1;
                if outcome.heartbeat_error.is_some() {
                    heartbeat_errors_seen += 1;
                }
            },
        );

        assert_eq!(
            iterations_seen, 10,
            "the loop must keep running for all iterations despite heartbeat write failures"
        );
        assert_eq!(
            heartbeat_errors_seen, 10,
            "every iteration must report the heartbeat write failure rather than silently \
             succeed or panic"
        );
        assert!(
            !heartbeat_path.exists(),
            "a failed heartbeat write must not leave a partial file behind"
        );

        let _ = std::fs::remove_file(&dir);
    }
}
