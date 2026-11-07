//! Host-load gate and background priority for unprompted scans (HORO-2361).
//!
//! A disk scan must not add to a host that is already struggling: on the
//! machine that motivated this, scans of the same trees made tool calls take
//! minutes while `syspolicyd` held a core at 270%. An *unattended* run (the
//! answer to a disk-pressure alert that nobody asked for) therefore lowers
//! its own priority and declines to start while the one-minute load average
//! per core is above a ceiling.
//!
//! Declining a scan is not a safety claim in either direction: it deletes
//! nothing, classifies nothing, and the pressure poller raises the alert
//! again on its next sample. Unmeasurable load is admitted, because "could
//! not read the load" is not evidence that the host is busy.

/// One-minute load average per logical core above which an unattended scan
/// does not start. An engineering default, not a measured number.
pub const DEFAULT_LOAD_CEILING_PER_CORE: f64 = 1.5;

/// Environment variable that overrides [`DEFAULT_LOAD_CEILING_PER_CORE`]
/// (a positive, finite number of load-per-core).
pub const LOAD_CEILING_ENV: &str = "GLOMERIS_SCAN_LOAD_CEILING";

/// The gate's answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadGate {
    Admit {
        load_per_core: f64,
    },
    Busy {
        load_per_core: f64,
        ceiling: f64,
    },
    /// The load average could not be read; admitted.
    Unmeasured,
}

/// Pure decision: `load_one_minute` spread over `cores`, against `ceiling`.
pub fn evaluate(load_one_minute: f64, cores: usize, ceiling: f64) -> LoadGate {
    let load_per_core = load_one_minute / cores.max(1) as f64;
    if !load_per_core.is_finite() || load_per_core < 0.0 {
        LoadGate::Unmeasured
    } else if load_per_core > ceiling {
        LoadGate::Busy {
            load_per_core,
            ceiling,
        }
    } else {
        LoadGate::Admit { load_per_core }
    }
}

/// Parses a ceiling override; anything that is not a positive finite number
/// falls back to the default rather than silently disabling the gate.
pub fn ceiling_from(raw: Option<&str>) -> f64 {
    raw.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_LOAD_CEILING_PER_CORE)
}

/// The one-minute load average, when the platform reports one.
#[cfg(unix)]
pub fn current_load_one_minute() -> Option<f64> {
    let mut averages = [0f64; 3];
    // SAFETY: `averages` is a valid, writable buffer of exactly 3 doubles.
    let written = unsafe { nix::libc::getloadavg(averages.as_mut_ptr(), 3) };
    (written >= 1).then_some(averages[0])
}

#[cfg(not(unix))]
pub fn current_load_one_minute() -> Option<f64> {
    None
}

/// Checks the live host against the configured ceiling.
pub fn check_host_load() -> LoadGate {
    let ceiling = ceiling_from(std::env::var(LOAD_CEILING_ENV).ok().as_deref());
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    match current_load_one_minute() {
        Some(load) => evaluate(load, cores, ceiling),
        None => LoadGate::Unmeasured,
    }
}

/// Puts this process in the background priority band (macOS
/// `PRIO_DARWIN_BG`: lowest CPU, I/O and network priority), so a scan yields
/// to interactive work. Returns whether the request was accepted; a refusal
/// is not an error, the scan simply runs at normal priority.
#[cfg(target_os = "macos")]
pub fn enter_background_qos() -> bool {
    // SAFETY: plain syscall on the calling process; no pointers involved.
    unsafe {
        nix::libc::setpriority(nix::libc::PRIO_DARWIN_PROCESS, 0, nix::libc::PRIO_DARWIN_BG) == 0
    }
}

#[cfg(not(target_os = "macos"))]
pub fn enter_background_qos() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_only_when_load_per_core_exceeds_the_ceiling() {
        assert_eq!(
            evaluate(24.0, 8, 1.5),
            LoadGate::Busy {
                load_per_core: 3.0,
                ceiling: 1.5
            }
        );
        assert_eq!(
            evaluate(12.0, 8, 1.5),
            LoadGate::Admit { load_per_core: 1.5 }
        );
        assert_eq!(
            evaluate(2.0, 8, 1.5),
            LoadGate::Admit {
                load_per_core: 0.25
            }
        );
    }

    #[test]
    fn nonsense_load_is_unmeasured_not_busy() {
        assert_eq!(evaluate(f64::NAN, 8, 1.5), LoadGate::Unmeasured);
        assert_eq!(evaluate(-1.0, 8, 1.5), LoadGate::Unmeasured);
        assert_eq!(evaluate(f64::INFINITY, 8, 1.5), LoadGate::Unmeasured);
    }

    #[test]
    fn zero_cores_is_treated_as_one() {
        assert_eq!(
            evaluate(3.0, 0, 1.5),
            LoadGate::Busy {
                load_per_core: 3.0,
                ceiling: 1.5
            }
        );
    }

    #[test]
    fn ceiling_override_must_be_positive_and_finite() {
        assert_eq!(ceiling_from(Some("4")), 4.0);
        assert_eq!(ceiling_from(Some(" 0.5 ")), 0.5);
        for bad in ["0", "-2", "nan", "inf", "fast", ""] {
            assert_eq!(
                ceiling_from(Some(bad)),
                DEFAULT_LOAD_CEILING_PER_CORE,
                "{bad:?}"
            );
        }
        assert_eq!(ceiling_from(None), DEFAULT_LOAD_CEILING_PER_CORE);
    }

    #[cfg(unix)]
    #[test]
    fn the_live_host_reports_a_load_average() {
        let load = current_load_one_minute().expect("getloadavg on unix");
        assert!(load.is_finite() && load >= 0.0);
    }

    /// The priority change is process-wide, so it is checked in a child copy of
    /// this test binary and never in the shared test process.
    #[cfg(target_os = "macos")]
    #[test]
    fn background_qos_is_applied_to_the_process() {
        const CHILD: &str = "GLOMERIS_QOS_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(enter_background_qos(), "the background band was refused");
            // SAFETY: plain syscall on the calling process.
            let band = unsafe { nix::libc::getpriority(nix::libc::PRIO_DARWIN_PROCESS, 0) };
            assert_eq!(band, 1, "the process is not in the background band");
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .args([
                "--exact",
                "scanner::host_load::tests::background_qos_is_applied_to_the_process",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn child test");
        assert!(status.success(), "child reported a failure");
    }
}
