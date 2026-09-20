//! Storage-impact tiers (HORO-1307): "is this candidate an ordinary bit of
//! housekeeping, or one of the few things on this machine actually worth
//! looking at?"
//!
//! # Why this lives in Rust and not in the GUI
//!
//! A tier is a product judgment about what counts as a lot of space, which
//! makes it domain semantics rather than presentation. Putting it in the
//! menu-bar app would mean the CLI and the GUI could disagree about which
//! candidates matter, and it would put a threshold in the thin client that
//! the standing rule in `macos/GlomerisMenuBar/Sources/GlomerisMenuBarApp.swift`
//! keeps out of it. HORO-1307's AC #7 says this explicitly: thresholds are
//! unit-tested and not hardcoded in Swift when they are product semantics.
//! Swift receives the already-decided `impact_tier` token and picks wording
//! and emphasis for it, exactly as it already does for `policy_label`.
//!
//! # A tier is NOT a safety signal
//!
//! This is the whole reason the type exists separately from
//! [`crate::reporting::PolicyLabel`]. A [`StorageImpactTier::Large`]
//! candidate may be perfectly safe to reclaim — in fact a large `AUTO_SAFE`
//! candidate is the single best thing a user can be shown. A
//! [`StorageImpactTier::Normal`] one may be `PROTECTED` and completely
//! untouchable. The two axes are independent, and nothing in this module
//! reads or produces a policy label.
//!
//! # Why the thresholds are what they are
//!
//! Two scales, because either one alone is wrong in a common case:
//!
//! * **Absolute bytes.** 12 GB of Rust build output is worth surfacing
//!   whether the disk is 512 GB or 8 TB — it is a real amount of a real
//!   developer's real machine.
//! * **A fraction of *free* space.** On a nearly-full disk with 1.5 GB
//!   free, a 400 MB cache is a quarter of everything the user has left.
//!   Absolute thresholds would file that under "normal" precisely when it
//!   matters most.
//!
//! The fraction is measured against **free** space rather than total
//! capacity on purpose: the problem a user opens Glomeris with is "I am
//! running out of room", and "this would give you back a quarter of your
//! remaining headroom" speaks to that. A percentage of a 4 TB total would
//! classify almost everything as trivial.
//!
//! The relative scale can only ever **escalate** a tier, never lower one.
//! A 30 GB directory on a disk with 2 TB free is still 30 GB, and telling
//! the user it is unremarkable because their disk is big would be a way of
//! hiding the largest thing on the list.

/// How much a candidate's reclaimable size is worth the user's attention.
///
/// Ordered least-to-most notable so `#[derive(PartialOrd)]` gives the
/// escalate-only rule below its meaning directly (`max` of two tiers).
/// [`StorageImpactTier::Unknown`] sorts lowest deliberately: a size that
/// could not be measured is not a small size, but it is also not something
/// to draw a user's eye to over a measured, actionable figure. See
/// [`crate::reporting::ranking`] for how it is ordered in a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageImpactTier {
    /// No reclaimable-bytes figure at all. Not zero, and not small —
    /// unmeasured.
    Unknown,
    /// A real but ordinary amount. Most candidates on a healthy machine.
    Normal,
    /// Enough to be worth a glance.
    Notable,
    /// Among the things actually worth acting on.
    Large,
}

impl StorageImpactTier {
    /// The token emitted in `--json` output and consumed by the menu-bar
    /// app's display vocabulary. Stable wire format: adding a variant here
    /// means adding wording in `GlomerisVocabulary.swift`, which
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` checks mechanically.
    pub fn as_str(&self) -> &'static str {
        match self {
            StorageImpactTier::Unknown => "unknown",
            StorageImpactTier::Normal => "normal",
            StorageImpactTier::Notable => "notable",
            StorageImpactTier::Large => "large",
        }
    }
}

/// The threshold model itself, in one place, so the numbers are reviewable
/// and overridable rather than scattered through call sites as literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpactThresholds {
    /// At or above this many bytes, a candidate is at least
    /// [`StorageImpactTier::Notable`].
    pub notable_bytes: u64,
    /// At or above this many bytes, a candidate is
    /// [`StorageImpactTier::Large`].
    pub large_bytes: u64,
    /// At or above this percentage of currently-free space, a candidate is
    /// at least [`StorageImpactTier::Notable`].
    pub notable_free_percent: u32,
    /// At or above this percentage of currently-free space, a candidate is
    /// [`StorageImpactTier::Large`].
    pub large_free_percent: u32,
}

impl Default for ImpactThresholds {
    /// 1 GiB / 10 GiB absolute, 5% / 20% of remaining free space.
    ///
    /// Calibrated against what the detectors actually find on a developer
    /// machine: a single `node_modules` or Cargo registry cache lands in
    /// the high hundreds of MB to low GB, so 1 GiB is the point where a
    /// candidate stops being one of dozens of similar things; a per-project
    /// Cargo `target/` or Xcode DerivedData directory reaches tens of GB,
    /// so 10 GiB separates "worth a glance" from "this is the answer".
    fn default() -> Self {
        Self {
            notable_bytes: 1024 * 1024 * 1024,
            large_bytes: 10 * 1024 * 1024 * 1024,
            notable_free_percent: 5,
            large_free_percent: 20,
        }
    }
}

/// What is known about the filesystem the candidates live on, for the
/// relative half of the threshold model.
///
/// `free_bytes` is `None` wherever the caller cannot cheaply and reliably
/// obtain it — a non-macOS build, or a `statfs` that failed. That is a
/// real, expected case and not an error: the absolute scale alone still
/// produces an honest tier, and [`classify_impact`] simply skips the
/// relative escalation. It must never fall back to a guessed capacity,
/// which would produce a confidently wrong tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImpactContext {
    pub free_bytes: Option<u64>,
}

impl ImpactContext {
    pub fn with_free_bytes(free_bytes: u64) -> Self {
        Self {
            free_bytes: Some(free_bytes),
        }
    }
}

/// Classify one candidate's reclaimable size into a tier.
///
/// `reclaimable_bytes` is `None` when nothing could be measured, which
/// yields [`StorageImpactTier::Unknown`] rather than a small tier —
/// "we do not know" and "not much" are different claims, and the second
/// one would be a fabrication.
///
/// A lower-bound estimate (see
/// [`crate::reporting::dto::DetectCandidateReport::reclaimable_bytes_is_lower_bound`])
/// is classified on the bytes actually observed, which can only *understate*
/// the tier — a truncated scan that found 9 GiB is reported as `Notable`
/// even if the real figure would have been `Large`. That is the honest
/// direction to err in: the alternative is inflating a tier from a number
/// nobody measured. The lower-bound flag travels alongside the tier in the
/// report so the understatement is visible rather than silent, which is
/// what HORO-1307's AC #2 asks for.
pub fn classify_impact(
    reclaimable_bytes: Option<u64>,
    context: ImpactContext,
    thresholds: &ImpactThresholds,
) -> StorageImpactTier {
    let Some(bytes) = reclaimable_bytes else {
        return StorageImpactTier::Unknown;
    };

    let absolute = if bytes >= thresholds.large_bytes {
        StorageImpactTier::Large
    } else if bytes >= thresholds.notable_bytes {
        StorageImpactTier::Notable
    } else {
        StorageImpactTier::Normal
    };

    // Escalate-only: see the module header. `max` over a tier ordering that
    // is deliberately least-to-most notable is the whole implementation of
    // that rule.
    absolute.max(relative_tier(bytes, context, thresholds))
}

/// The relative half of the model. Returns [`StorageImpactTier::Normal`]
/// (i.e. "nothing to add") whenever free space is unknown or zero, so the
/// caller's `max` leaves the absolute tier untouched.
fn relative_tier(
    bytes: u64,
    context: ImpactContext,
    thresholds: &ImpactThresholds,
) -> StorageImpactTier {
    // Zero free space is not a division-by-zero case to paper over: it is
    // EMERGENCY, and every candidate is then infinitely large relative to
    // what is left. Saying so would make every row shout at once, which
    // conveys nothing. The absolute scale still ranks them usefully.
    let Some(free) = context.free_bytes.filter(|free| *free > 0) else {
        return StorageImpactTier::Normal;
    };

    // Percent as an integer comparison rather than floating-point: `bytes`
    // can be terabytes, so `bytes * 100` is computed in u128 to remove any
    // question of overflow at the top of the range.
    let percent_of_free = (u128::from(bytes) * 100) / u128::from(free);

    if percent_of_free >= u128::from(thresholds.large_free_percent) {
        StorageImpactTier::Large
    } else if percent_of_free >= u128::from(thresholds.notable_free_percent) {
        StorageImpactTier::Notable
    } else {
        StorageImpactTier::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    fn no_disk_context() -> ImpactContext {
        ImpactContext::default()
    }

    // ---------------------------------------------------------------
    // The absolute scale
    // ---------------------------------------------------------------

    #[test]
    fn unmeasured_size_is_unknown_and_not_small() {
        let tier = classify_impact(None, no_disk_context(), &ImpactThresholds::default());
        assert_eq!(tier, StorageImpactTier::Unknown);
        assert_ne!(
            tier,
            StorageImpactTier::Normal,
            "an unmeasured size must not be reported as an ordinary small one"
        );
    }

    #[test]
    fn zero_bytes_is_normal_not_unknown() {
        // A measured zero IS information: there is genuinely nothing here.
        assert_eq!(
            classify_impact(Some(0), no_disk_context(), &ImpactThresholds::default()),
            StorageImpactTier::Normal
        );
    }

    #[test]
    fn absolute_thresholds_are_inclusive_at_their_boundary() {
        let t = ImpactThresholds::default();
        assert_eq!(
            classify_impact(Some(t.notable_bytes - 1), no_disk_context(), &t),
            StorageImpactTier::Normal
        );
        assert_eq!(
            classify_impact(Some(t.notable_bytes), no_disk_context(), &t),
            StorageImpactTier::Notable
        );
        assert_eq!(
            classify_impact(Some(t.large_bytes - 1), no_disk_context(), &t),
            StorageImpactTier::Notable
        );
        assert_eq!(
            classify_impact(Some(t.large_bytes), no_disk_context(), &t),
            StorageImpactTier::Large
        );
    }

    #[test]
    fn a_very_large_candidate_stays_large() {
        assert_eq!(
            classify_impact(
                Some(3 * 1024 * GIB),
                no_disk_context(),
                &ImpactThresholds::default()
            ),
            StorageImpactTier::Large
        );
    }

    // ---------------------------------------------------------------
    // The relative scale
    // ---------------------------------------------------------------

    #[test]
    fn a_small_candidate_escalates_when_free_space_is_nearly_gone() {
        // 400 MiB is "normal" in absolute terms, and a quarter of what this
        // user has left. This is the case the relative scale exists for.
        let t = ImpactThresholds::default();
        let bytes = 400 * MIB;
        assert_eq!(
            classify_impact(Some(bytes), no_disk_context(), &t),
            StorageImpactTier::Normal,
            "absolute scale alone"
        );
        assert_eq!(
            classify_impact(Some(bytes), ImpactContext::with_free_bytes(1600 * MIB), &t),
            StorageImpactTier::Large,
            "25% of remaining free space is the largest thing this user can be told about"
        );
    }

    #[test]
    fn relative_scale_never_demotes_a_large_candidate() {
        // 30 GiB is 1.5% of a 2 TiB free disk. It is still 30 GiB.
        let t = ImpactThresholds::default();
        assert_eq!(
            classify_impact(
                Some(30 * GIB),
                ImpactContext::with_free_bytes(2048 * GIB),
                &t
            ),
            StorageImpactTier::Large
        );
    }

    #[test]
    fn relative_boundaries_are_inclusive() {
        let t = ImpactThresholds::default();
        let free = 100 * GIB;
        // Exactly 5% of free, well under the 1 GiB absolute notable floor
        // only if free is small; here 5 GiB is over it, so use a threshold
        // set with no absolute component to isolate the relative rule.
        let relative_only = ImpactThresholds {
            notable_bytes: u64::MAX,
            large_bytes: u64::MAX,
            ..t
        };
        assert_eq!(
            classify_impact(
                Some(free / 100 * 5),
                ImpactContext::with_free_bytes(free),
                &relative_only
            ),
            StorageImpactTier::Notable
        );
        assert_eq!(
            classify_impact(
                Some(free / 100 * 20),
                ImpactContext::with_free_bytes(free),
                &relative_only
            ),
            StorageImpactTier::Large
        );
        assert_eq!(
            classify_impact(
                Some(free / 100 * 4),
                ImpactContext::with_free_bytes(free),
                &relative_only
            ),
            StorageImpactTier::Normal
        );
    }

    #[test]
    fn unknown_free_space_falls_back_to_the_absolute_scale_only() {
        let t = ImpactThresholds::default();
        assert_eq!(
            classify_impact(Some(400 * MIB), ImpactContext { free_bytes: None }, &t),
            StorageImpactTier::Normal,
            "with no disk context the relative rule must contribute nothing at all"
        );
    }

    #[test]
    fn zero_free_space_does_not_make_every_candidate_large() {
        // Division by zero avoided, and the product reason for avoiding it:
        // if everything shouts, nothing is ranked.
        let t = ImpactThresholds::default();
        assert_eq!(
            classify_impact(Some(1), ImpactContext::with_free_bytes(0), &t),
            StorageImpactTier::Normal
        );
        assert_eq!(
            classify_impact(Some(50 * GIB), ImpactContext::with_free_bytes(0), &t),
            StorageImpactTier::Large,
            "the absolute scale must still rank a genuinely huge candidate"
        );
    }

    #[test]
    fn a_terabyte_scale_candidate_does_not_overflow_the_percentage() {
        // `bytes * 100` in u64 would overflow around 184 PiB; this is the
        // reason the arithmetic is done in u128.
        let t = ImpactThresholds {
            notable_bytes: u64::MAX,
            large_bytes: u64::MAX,
            ..ImpactThresholds::default()
        };
        assert_eq!(
            classify_impact(
                Some(u64::MAX / 2),
                ImpactContext::with_free_bytes(u64::MAX),
                &t
            ),
            StorageImpactTier::Large
        );
    }

    // ---------------------------------------------------------------
    // The token contract
    // ---------------------------------------------------------------

    #[test]
    fn every_tier_has_a_distinct_stable_token() {
        let tokens = [
            StorageImpactTier::Unknown.as_str(),
            StorageImpactTier::Normal.as_str(),
            StorageImpactTier::Notable.as_str(),
            StorageImpactTier::Large.as_str(),
        ];
        assert_eq!(tokens, ["unknown", "normal", "notable", "large"]);
        let mut unique = tokens.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), tokens.len(), "two tiers share a token");
    }

    #[test]
    fn tier_ordering_is_least_to_most_notable() {
        // The escalate-only rule is implemented as `max` over this ordering,
        // so the ordering itself is load-bearing.
        assert!(StorageImpactTier::Unknown < StorageImpactTier::Normal);
        assert!(StorageImpactTier::Normal < StorageImpactTier::Notable);
        assert!(StorageImpactTier::Notable < StorageImpactTier::Large);
    }
}
