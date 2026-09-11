//! Bounded closed-loop disk recovery (HORO-952): the orchestration behind
//! `glomeris free --target <threshold>`.
//!
//! This module wires together, without reimplementing any of them:
//! [`crate::monitor::FsStat`] (measurement), [`crate::detectors::DetectorRegistry`]
//! (discovery), [`crate::evidence::correlate::EvidenceCollector`]
//! (correlation refresh), [`crate::policy::classify`]/[`crate::policy::authorize`]
//! (the AUTO_SAFE/ASK/PROTECTED decision), and [`crate::executor::execute`]
//! (HORO-951's fully-hardened, TOCTOU-safe deletion). The loop itself adds
//! only orchestration and bookkeeping — target/budget checks, one-candidate-
//! per-iteration selection, and a no-progress guard.
//!
//! Placement note: this lives under `crate::executor` (rather than a new
//! top-level `crate::recovery` module) because it is a thin caller of
//! `executor::execute` plus the other already-hardened modules — it does
//! not introduce a new safety-critical primitive of its own, so it does
//! not need its own top-level module boundary.
//!
//! Canonical safety invariant, unchanged: AI can recommend. Policy
//! decides. Executor verifies. Filesystem reality wins. This loop never
//! bypasses `policy::classify`/`policy::authorize`, never constructs an
//! `Approval` itself, and never touches the filesystem directly — every
//! mutation happens inside a real `executor::execute` call.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::actions::ActionRegistry;
use crate::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, NativeCleanup, ResourceId};
use crate::evidence::probe::ProbeOutcome;
use crate::executor::{execute, ExecutionOutcome};
use crate::monitor::{Clock, FsStat, FsUsage};
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, UserConsent};

/// Time budget applied to each candidate's correlation refresh pass
/// (step 5 of the loop). Mirrors `executor::REVALIDATION_TIMEOUT`'s
/// reasoning: this runs synchronously inside a bounded recovery loop, so
/// it must not stall indefinitely. Not reused directly because that
/// constant is private to `executor`.
const CANDIDATE_CORRELATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Consecutive "executed successfully but freed nothing measurable"
/// iterations before the loop gives up rather than spinning forever (see
/// [`StopReason::NoProgress`]).
const NO_PROGRESS_STREAK_THRESHOLD: u32 = 2;

/// Abstraction over "what time is it right now" for the wall-clock
/// timestamps [`crate::policy::classify`]/[`crate::policy::authorize`]
/// need (`Evidence::collected_at`, `UserConsent::granted_at`). Distinct
/// from [`crate::monitor::Clock`] (which is `Instant`-based and used here
/// only for the `max_duration` budget check) because `classify` takes a
/// `SystemTime`, and no existing trait in this codebase provides an
/// injectable `SystemTime` source.
pub trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// Real wall-clock time. Used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The disk-free goal a recovery run is trying to reach.
///
/// Both variants describe a target *state* of the filesystem (how much
/// free space should exist when the loop stops), not an amount to
/// reclaim — this is what makes "target already met, stop immediately"
/// (loop step 2) a coherent check before any action has run for either
/// variant. `AbsoluteBytes` byte multipliers used by the CLI parser (see
/// [`parse_free_target`]) are binary (1024-based: `5GB` == `5 * 1024^3`
/// bytes), matching the ticket's `5GB` / `5368709120` example pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FreeTarget {
    /// Target: at least this percentage (0.0..=100.0) of total capacity
    /// free.
    Percentage(f64),
    /// Target: at least this many bytes free.
    AbsoluteBytes(u64),
}

/// Bounds and behavior tunables for one recovery run.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    pub target: FreeTarget,
    pub max_iterations: u32,
    pub max_actions: u32,
    pub max_duration: Duration,
    /// If `false`, `Ask`-classified candidates are reported as declined/
    /// skipped rather than executed — there is no interactive prompt in
    /// this MVP (see module docs and the PR's "Known limitations": real
    /// interactive approval UX is HORO-955's job). If `true`, an `Ask`
    /// candidate is auto-approved by constructing a `UserConsent` from the
    /// exact evidence/fingerprint just observed, purely as an MVP
    /// simplification for non-interactive runs.
    pub auto_approve_ask: bool,
}

/// Why a recovery run stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// `RecoveryConfig::target` was met (checked at the top of an
    /// iteration, before any candidate is considered).
    TargetReached,
    /// No `AutoSafe` candidate remains, and either no `Ask` candidate
    /// remains or `auto_approve_ask` is `false`.
    SafeExhausted,
    /// `max_iterations`, `max_actions`, or `max_duration` was reached.
    BudgetExceeded,
    /// The last [`NO_PROGRESS_STREAK_THRESHOLD`] consecutive successfully
    /// executed actions each measured zero (or unmeasurable) actual
    /// reclaimed bytes.
    NoProgress,
    /// The initial or a subsequent disk-usage measurement itself failed.
    Error(String),
}

/// Summary of one recovery run.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub stop_reason: StopReason,
    pub iterations_run: u32,
    pub actions_executed: u32,
    /// `Ask` candidates that were found but not executed — either because
    /// `auto_approve_ask` was `false`, or because `policy::authorize`
    /// unexpectedly refused, or because the resource's action id could
    /// not be resolved. Also incremented for a candidate whose executed
    /// action came back `Failed`/`AbortedByRevalidation` (it is skipped,
    /// never retried, for the remainder of this run).
    pub actions_declined_or_skipped: u32,
    /// Sum of ACTUAL (not expected) reclaimed bytes across all executed
    /// actions, per [`crate::executor::ExecutionReport::actual_reclaimed_bytes`].
    pub total_bytes_freed: u64,
    pub started_free_bytes: u64,
    pub final_free_bytes: u64,
}

/// Is `usage` already at or past `target`? Shared by the loop's own
/// target check and available to callers (e.g. the CLI) that want to
/// report progress without duplicating the arithmetic.
pub fn target_met(usage: &FsUsage, target: &FreeTarget) -> bool {
    match target {
        FreeTarget::Percentage(pct) => {
            if usage.total_bytes == 0 {
                return true;
            }
            usage.free_bytes as f64 >= (pct / 100.0) * usage.total_bytes as f64
        }
        FreeTarget::AbsoluteBytes(bytes) => usage.free_bytes >= *bytes,
    }
}

/// Parses a `--target` CLI value into a [`FreeTarget`].
///
/// Format (deliberately minimal — see the PR's "Known limitations"):
/// - A trailing `%` is a [`FreeTarget::Percentage`], e.g. `"20%"`. Must
///   parse as a number in `0.0..=100.0`.
/// - Otherwise the value is a [`FreeTarget::AbsoluteBytes`]. An optional
///   case-insensitive `KB`/`MB`/`GB`/`TB` suffix uses binary (1024-based)
///   multipliers (`"5GB"` == `5 * 1024^3` bytes, matching the ticket's
///   `5GB`/`5368709120` example pair); a bare trailing `B` means "no
///   multiplier"; no suffix at all is also read as a raw byte count
///   (e.g. `"5368709120"`).
/// - No support for fractional shorthand combinations, negative values,
///   or any unit beyond TB.
pub fn parse_free_target(input: &str) -> Result<FreeTarget, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty --target value".to_string());
    }

    if let Some(pct) = trimmed.strip_suffix('%') {
        let value: f64 = pct
            .trim()
            .parse()
            .map_err(|_| format!("invalid percentage in --target value: {input}"))?;
        if !(0.0..=100.0).contains(&value) {
            return Err(format!("--target percentage out of range 0-100: {input}"));
        }
        return Ok(FreeTarget::Percentage(value));
    }

    let upper = trimmed.to_ascii_uppercase();
    let (numeric_part, multiplier): (&str, u64) = if let Some(p) = upper.strip_suffix("TB") {
        (p, 1024u64.pow(4))
    } else if let Some(p) = upper.strip_suffix("GB") {
        (p, 1024u64.pow(3))
    } else if let Some(p) = upper.strip_suffix("MB") {
        (p, 1024u64.pow(2))
    } else if let Some(p) = upper.strip_suffix("KB") {
        (p, 1024)
    } else if let Some(p) = upper.strip_suffix('B') {
        (p, 1)
    } else {
        (upper.as_str(), 1)
    };

    let value: f64 = numeric_part
        .trim()
        .parse()
        .map_err(|_| format!("invalid byte amount in --target value: {input}"))?;
    if value < 0.0 {
        return Err(format!(
            "--target byte amount must not be negative: {input}"
        ));
    }

    Ok(FreeTarget::AbsoluteBytes(
        (value * multiplier as f64) as u64,
    ))
}

#[cfg(test)]
mod parse_free_target_tests {
    use super::*;

    #[test]
    fn parses_percentage_form() {
        assert_eq!(parse_free_target("20%"), Ok(FreeTarget::Percentage(20.0)));
    }

    #[test]
    fn parses_percentage_with_fraction() {
        assert_eq!(parse_free_target("12.5%"), Ok(FreeTarget::Percentage(12.5)));
    }

    #[test]
    fn rejects_percentage_out_of_range() {
        assert!(parse_free_target("150%").is_err());
    }

    #[test]
    fn parses_raw_byte_count_with_no_suffix() {
        assert_eq!(
            parse_free_target("5368709120"),
            Ok(FreeTarget::AbsoluteBytes(5_368_709_120))
        );
    }

    #[test]
    fn parses_gb_suffix_as_binary_gigabytes() {
        assert_eq!(
            parse_free_target("5GB"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_lowercase_suffix() {
        assert_eq!(
            parse_free_target("5gb"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_kb_and_mb_and_tb_suffixes() {
        assert_eq!(
            parse_free_target("1KB"),
            Ok(FreeTarget::AbsoluteBytes(1024))
        );
        assert_eq!(
            parse_free_target("1MB"),
            Ok(FreeTarget::AbsoluteBytes(1024 * 1024))
        );
        assert_eq!(
            parse_free_target("1TB"),
            Ok(FreeTarget::AbsoluteBytes(1024u64.pow(4)))
        );
    }

    #[test]
    fn rejects_negative_byte_amount() {
        assert!(parse_free_target("-5GB").is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_free_target("").is_err());
        assert!(parse_free_target("   ").is_err());
    }

    #[test]
    fn rejects_garbage_input() {
        assert!(parse_free_target("not-a-target").is_err());
    }
}

#[cfg(test)]
mod types_tests {
    use super::*;

    #[test]
    fn percentage_target_met_when_free_fraction_meets_threshold() {
        let usage = FsUsage::new(100, 20);
        assert!(target_met(&usage, &FreeTarget::Percentage(20.0)));
        assert!(!target_met(&usage, &FreeTarget::Percentage(20.01)));
    }

    #[test]
    fn absolute_target_met_when_free_bytes_meets_threshold() {
        let usage = FsUsage::new(1_000, 500);
        assert!(target_met(&usage, &FreeTarget::AbsoluteBytes(500)));
        assert!(!target_met(&usage, &FreeTarget::AbsoluteBytes(501)));
    }

    #[test]
    fn percentage_target_on_zero_capacity_filesystem_is_trivially_met() {
        // Degenerate zero-total filesystem: nothing to free, so the
        // target can't meaningfully be unmet.
        let usage = FsUsage::new(0, 0);
        assert!(target_met(&usage, &FreeTarget::Percentage(50.0)));
    }
}
