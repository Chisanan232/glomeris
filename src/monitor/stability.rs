//! Honest stability analysis over a window of [`VolumeSample`]s (HORO-1827 /
//! ADR-0001 §4.4, §8).
//!
//! Pure and deterministic: every input is a slice of already-collected
//! samples (whose timestamps come from [`crate::monitor::clock::Clock`] at
//! collection time, never from this module), so a fake-clock test can feed
//! a scripted sequence of samples and get a reproducible verdict, with no
//! real sleeping.
//!
//! The one rule this module exists to enforce: **never fabricate a
//! stability verdict from an observation window that has not actually
//! elapsed 12 real hours.** [`StabilityVerdict::InsufficientObservation`] is
//! the honest answer for "too few samples", "too short a span", and
//! "nothing collected yet" alike — there is deliberately no code path that
//! produces [`StabilityVerdict::Analyzed`] any other way.

use super::volume_sample::VolumeSample;

/// Minimum real elapsed wall-clock time, in seconds, before a stability
/// verdict may be anything other than
/// [`StabilityVerdict::InsufficientObservation`]. 12 hours, per this
/// ticket's acceptance criteria.
pub const MIN_OBSERVATION_SECS: u64 = 12 * 60 * 60;

/// How much a volume's free space may wander within the window before it
/// stops counting as [`Trend::Healthy`] — expressed as a fraction of the
/// volume's total capacity, not a fixed byte count, so the same tolerance
/// is meaningful on a 256 GiB laptop and a 4 TiB workstation.
///
/// Exists specifically to absorb "APFS accounting differences": small,
/// benign wobbles in the free-byte count that are not a real change in
/// usage (space reclamation lag, metadata bookkeeping, purgeable space
/// changing size). Without this tolerance every such wobble would read as
/// [`Trend::Volatile`].
const STABILITY_TOLERANCE_FRACTION: f64 = 0.005;

/// How a volume's free space moved across the analyzed window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    /// Free space stayed within [`STABILITY_TOLERANCE_FRACTION`] of its
    /// starting value for the whole window.
    Healthy,
    /// Free space moved by more than the tolerance, but the window's net
    /// change does not dominate — it went up and down rather than trending
    /// one direction, e.g. builds running and then `cargo clean`ing.
    Volatile,
    /// Free space moved by more than the tolerance and the window's net
    /// change is a sustained decrease — the volume is filling.
    Falling,
}

/// A stability verdict for one window of same-volume samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StabilityAnalysis {
    pub volume_dev: u64,
    pub window_start_unix_secs: u64,
    pub window_end_unix_secs: u64,
    pub elapsed_secs: u64,
    pub min_free_bytes: u64,
    pub max_free_bytes: u64,
    pub start_free_bytes: u64,
    pub end_free_bytes: u64,
    /// Signed: negative means free space is falling (the volume is
    /// filling), positive means it is growing.
    pub rate_bytes_per_sec: f64,
    /// Number of gaps between consecutive samples materially larger than
    /// `expected_interval_secs` — a missed sampling tick, a sleeping
    /// machine, or a daemon restart.
    pub missing_intervals: u32,
    pub trend: Trend,
    /// Lowest free-byte reading seen in the window — the same value as
    /// `min_free_bytes`, named separately because "low watermark" is the
    /// term this ticket's acceptance criteria and the feasibility report
    /// use; kept as its own field so a caller reading only this one does
    /// not have to know it is derived from `min_free_bytes`.
    pub low_watermark_free_bytes: u64,
}

/// Why a stability verdict could not be produced — always
/// [`StabilityVerdict::InsufficientObservation`]'s payload, never collapsed
/// into a single boolean, so a caller can say *why* (too few samples vs.
/// not enough elapsed time vs. a volume swap truncated the usable window).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StabilityVerdict {
    /// Fewer than 12 real hours of same-volume samples are available yet.
    /// `elapsed_secs` is the actual elapsed span of the usable window (0 if
    /// there are fewer than two samples to span at all).
    InsufficientObservation {
        elapsed_secs: u64,
        required_secs: u64,
    },
    Analyzed(StabilityAnalysis),
}

impl StabilityVerdict {
    pub fn insufficient(elapsed_secs: u64) -> Self {
        Self::InsufficientObservation {
            elapsed_secs,
            required_secs: MIN_OBSERVATION_SECS,
        }
    }
}

/// Analyzes `samples` (oldest first, as [`super::volume_sample::VolumeSampleRing`]
/// stores them) and returns an honest verdict.
///
/// Only the trailing run of samples sharing the most recent sample's
/// `volume_dev` is considered — ADR-0001 §4.4: a `volume_dev` mismatch
/// means "different filesystem", so an older run against a different mount
/// cannot be stitched onto the current one into one continuous window.
/// This deliberately shrinks the usable window rather than erroring: it is
/// the same "a volume swap truncates the window, it does not invalidate the
/// whole history" judgment call ADR-0001 §8 makes for other gaps.
pub fn analyze(samples: &[VolumeSample], expected_interval_secs: u64) -> StabilityVerdict {
    let Some(last) = samples.last() else {
        return StabilityVerdict::insufficient(0);
    };
    let dev = last.volume_dev;

    let mut start_idx = samples.len() - 1;
    for (idx, sample) in samples.iter().enumerate().rev() {
        if sample.volume_dev != dev {
            break;
        }
        start_idx = idx;
    }
    let window = &samples[start_idx..];

    if window.len() < 2 {
        return StabilityVerdict::insufficient(0);
    }

    let window_start = window.first().expect("checked len >= 2");
    let window_end = window.last().expect("checked len >= 2");
    let elapsed_secs = window_end.unix_secs.saturating_sub(window_start.unix_secs);

    if elapsed_secs < MIN_OBSERVATION_SECS {
        return StabilityVerdict::insufficient(elapsed_secs);
    }

    let min_free_bytes = window.iter().map(|s| s.free_bytes).min().unwrap_or(0);
    let max_free_bytes = window.iter().map(|s| s.free_bytes).max().unwrap_or(0);
    let start_free_bytes = window_start.free_bytes;
    let end_free_bytes = window_end.free_bytes;

    let net_change = end_free_bytes as i64 - start_free_bytes as i64;
    let rate_bytes_per_sec = if elapsed_secs == 0 {
        0.0
    } else {
        net_change as f64 / elapsed_secs as f64
    };

    let total_bytes_reference = window_end.total_bytes.max(1);
    let tolerance_bytes = (total_bytes_reference as f64 * STABILITY_TOLERANCE_FRACTION) as i64;
    let deviation = max_free_bytes as i64 - min_free_bytes as i64;
    let trend = if deviation <= tolerance_bytes {
        Trend::Healthy
    } else if net_change <= -tolerance_bytes {
        Trend::Falling
    } else {
        Trend::Volatile
    };

    let missing_intervals = count_missing_intervals(window, expected_interval_secs);

    StabilityVerdict::Analyzed(StabilityAnalysis {
        volume_dev: dev,
        window_start_unix_secs: window_start.unix_secs,
        window_end_unix_secs: window_end.unix_secs,
        elapsed_secs,
        min_free_bytes,
        max_free_bytes,
        start_free_bytes,
        end_free_bytes,
        rate_bytes_per_sec,
        missing_intervals,
        trend,
        low_watermark_free_bytes: min_free_bytes,
    })
}

/// Counts gaps between consecutive samples materially larger than
/// `expected_interval_secs` (more than 1.5x), each counted as
/// `gap / expected_interval_secs - 1` missed ticks — so a gap of exactly
/// one missed sample (roughly 2x the expected interval) counts as one
/// missing interval, and a longer sleep/outage counts proportionally more.
fn count_missing_intervals(window: &[VolumeSample], expected_interval_secs: u64) -> u32 {
    if expected_interval_secs == 0 {
        return 0;
    }
    let threshold = expected_interval_secs + expected_interval_secs / 2;
    let mut missing = 0u32;
    for pair in window.windows(2) {
        let gap = pair[1].unix_secs.saturating_sub(pair[0].unix_secs);
        if gap > threshold {
            let ticks = gap / expected_interval_secs;
            missing = missing.saturating_add((ticks.saturating_sub(1)).max(1) as u32);
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIFTEEN_MIN: u64 = 15 * 60;

    fn sample(secs: u64, free: u64) -> VolumeSample {
        VolumeSample::new(secs, 7, 1_000_000_000_000, free)
    }

    fn sample_dev(secs: u64, dev: u64, free: u64) -> VolumeSample {
        VolumeSample::new(secs, dev, 1_000_000_000_000, free)
    }

    /// 12h at 15-minute cadence, perfectly flat free space.
    fn healthy_window() -> Vec<VolumeSample> {
        (0..=48)
            .map(|i| sample(i * FIFTEEN_MIN, 500_000_000_000))
            .collect()
    }

    #[test]
    fn fewer_than_two_samples_is_insufficient_observation() {
        assert_eq!(analyze(&[], FIFTEEN_MIN), StabilityVerdict::insufficient(0));
        assert_eq!(
            analyze(&[sample(0, 100)], FIFTEEN_MIN),
            StabilityVerdict::insufficient(0)
        );
    }

    #[test]
    fn a_window_under_12h_is_insufficient_observation_even_with_many_samples() {
        // 100 samples, one minute apart: plenty of samples, nowhere near
        // 12h of actual elapsed time. Must not fabricate a verdict.
        let samples: Vec<VolumeSample> = (0..100).map(|i| sample(i * 60, 500)).collect();
        let verdict = analyze(&samples, 60);
        match verdict {
            StabilityVerdict::InsufficientObservation { elapsed_secs, .. } => {
                assert_eq!(elapsed_secs, 99 * 60);
                assert!(elapsed_secs < MIN_OBSERVATION_SECS);
            }
            other => panic!("expected InsufficientObservation, got {other:?}"),
        }
    }

    #[test]
    fn exactly_12h_of_flat_samples_is_healthy() {
        let samples = healthy_window();
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => {
                assert_eq!(a.trend, Trend::Healthy);
                assert_eq!(a.elapsed_secs, MIN_OBSERVATION_SECS);
                assert_eq!(a.missing_intervals, 0);
                assert_eq!(a.start_free_bytes, 500_000_000_000);
                assert_eq!(a.end_free_bytes, 500_000_000_000);
            }
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn apfs_accounting_noise_within_tolerance_still_reads_healthy() {
        // +-0.1% wobble around a stable baseline: must not be misread as
        // Volatile or Falling.
        let mut samples = Vec::new();
        for i in 0..=48u64 {
            let wobble = if i % 2 == 0 { 0 } else { 500_000_000 }; // 0.05% of 1TB
            samples.push(sample(i * FIFTEEN_MIN, 500_000_000_000 + wobble));
        }
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => assert_eq!(a.trend, Trend::Healthy),
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn volatile_window_goes_up_and_down_without_a_dominant_trend() {
        let mut samples = Vec::new();
        for i in 0..=48u64 {
            let free = if i % 2 == 0 {
                500_000_000_000
            } else {
                450_000_000_000
            };
            samples.push(sample(i * FIFTEEN_MIN, free));
        }
        // Ends back where it started -> net change ~0, but large deviation.
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => assert_eq!(a.trend, Trend::Volatile),
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn falling_window_sustained_decrease_is_falling() {
        let samples: Vec<VolumeSample> = (0..=48u64)
            .map(|i| sample(i * FIFTEEN_MIN, 500_000_000_000 - i * 1_000_000_000))
            .collect();
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => {
                assert_eq!(a.trend, Trend::Falling);
                assert!(a.rate_bytes_per_sec < 0.0);
            }
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn rapidly_growing_cargo_target_reads_as_falling_free_space() {
        // Free space drops fast and monotonically, as a target/ directory
        // fills during a long build.
        let samples: Vec<VolumeSample> = (0..=48u64)
            .map(|i| sample(i * FIFTEEN_MIN, 500_000_000_000 - i * 5_000_000_000))
            .collect();
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => {
                assert_eq!(a.trend, Trend::Falling);
                assert_eq!(a.low_watermark_free_bytes, a.end_free_bytes);
            }
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn a_missed_sample_is_detected_as_one_missing_interval() {
        let mut samples = healthy_window();
        // Remove the sample at index 24 (one tick), leaving a ~30-minute gap.
        samples.remove(24);
        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => assert_eq!(a.missing_intervals, 1),
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn a_volume_dev_change_truncates_the_window_to_the_new_volume_only() {
        // An hour of samples against dev 1 (not enough on its own), then a
        // volume swap, then 12h against dev 2. Only the dev-2 run counts.
        let mut samples: Vec<VolumeSample> = (0..4)
            .map(|i| sample_dev(i * FIFTEEN_MIN, 1, 100))
            .collect();
        let offset = samples.last().unwrap().unix_secs + FIFTEEN_MIN;
        samples.extend((0..=48u64).map(|i| sample_dev(offset + i * FIFTEEN_MIN, 2, 200)));

        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::Analyzed(a) => {
                assert_eq!(a.volume_dev, 2);
                assert_eq!(a.elapsed_secs, MIN_OBSERVATION_SECS);
            }
            other => panic!("expected Analyzed, got {other:?}"),
        }
    }

    #[test]
    fn a_volume_dev_change_too_recent_to_meet_12h_is_insufficient_observation() {
        let mut samples: Vec<VolumeSample> = (0..=48u64)
            .map(|i| sample_dev(i * FIFTEEN_MIN, 1, 100))
            .collect();
        let offset = samples.last().unwrap().unix_secs + FIFTEEN_MIN;
        samples.push(sample_dev(offset, 2, 200));
        samples.push(sample_dev(offset + FIFTEEN_MIN, 2, 200));

        let verdict = analyze(&samples, FIFTEEN_MIN);
        match verdict {
            StabilityVerdict::InsufficientObservation { elapsed_secs, .. } => {
                assert_eq!(elapsed_secs, FIFTEEN_MIN);
            }
            other => panic!("expected InsufficientObservation, got {other:?}"),
        }
    }
}
