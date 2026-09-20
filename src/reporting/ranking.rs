//! Canonical candidate ordering (HORO-1307).
//!
//! # The defect this fixes
//!
//! Before this module, `detect` emitted candidates in whatever order the
//! detectors happened to be registered in, flattened detector-by-detector
//! (see [`crate::cli::discover_and_classify_with_progress`]). Nothing
//! anywhere sorted them. So a 40 GB Cargo `target/` directory could — and
//! routinely did — render below a 2 MB npm cache, purely because the node
//! detector runs first. Both the text output and the menu-bar app inherited
//! that order, and the menu-bar app deliberately did not fix it locally,
//! because "which candidate matters most" is a product judgment and the app
//! is a thin client.
//!
//! # Where the ordering is applied, and why there
//!
//! Inside [`crate::cli::build_detect_report`], the single point where the
//! `detect` report is assembled. Both the human-readable output and
//! `--json` are rendered from that one report, so ordering there means they
//! cannot disagree — and a caller cannot accidentally get the unsorted
//! order by taking a different path to the same data.
//!
//! # The ordering, and the reasoning for each tie-break
//!
//! Descending priority:
//!
//! 1. **Measured sizes before unmeasured ones.** A candidate whose size
//!    could not be determined is not "big" and not "small"; it is unknown.
//!    Floating it to the top would push the largest actionable item down
//!    the list in favour of something we cannot describe, which is the
//!    opposite of helping. This is HORO-1307's AC #6: partial evidence must
//!    not outrank actionable findings.
//! 2. **Larger reclaimable size first.** The list's job is "what is worth
//!    looking at".
//! 3. **On equal bytes, a lower bound outranks an exact figure.** `≥ 5 GB`
//!    means *at least* 5 GB, so it is greater than or equal to an exact
//!    5 GB and belongs no lower. This is the ordering half of AC #2's
//!    "lower-bound estimates are handled visibly and correctly".
//! 4. **Then `resource_id` ascending.** Purely for determinism (AC #1):
//!    without it, two same-size candidates could swap places between runs
//!    and the list would appear to shuffle for no reason. Path order is
//!    also the least surprising fallback, since it groups candidates from
//!    the same project together.
//!
//! # What this is deliberately NOT
//!
//! It is not a safety ranking. Policy label is not an input here and must
//! not become one. A `PROTECTED` candidate is not demoted for being
//! protected, and an `AUTO_SAFE` one is not promoted for being safe:
//! demoting protected items would quietly hide from the user the fact that
//! something large on their disk is off-limits, and promoting safe ones
//! would turn the list into a work queue that implies "do these". The list
//! answers "what is big", the safety badge answers "what may be done about
//! it", and keeping those independent is the same separation-of-axes rule
//! the GUI vocabulary is built on.

use std::cmp::Ordering;

use crate::reporting::dto::DetectCandidateReport;

/// Just the fields the ordering reads, so the rule can be unit-tested
/// without building whole reports — and so it is obvious by inspection that
/// policy label, reasons and executability are not inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankInputs<'a> {
    pub reclaimable_bytes: Option<u64>,
    pub reclaimable_bytes_is_lower_bound: bool,
    pub resource_id: &'a str,
}

/// Compare two candidates by the canonical ordering documented above.
///
/// Total and antisymmetric: `resource_id` is unique per candidate within a
/// report, so this never returns [`Ordering::Equal`] for two distinct
/// candidates, which is what makes the resulting order reproducible.
pub fn compare(a: RankInputs<'_>, b: RankInputs<'_>) -> Ordering {
    // 1. Measured before unmeasured. `Option`'s own ordering puts `None`
    //    first, which is the wrong end, hence the explicit match rather
    //    than a derived comparison.
    match (a.reclaimable_bytes, b.reclaimable_bytes) {
        (Some(a_bytes), Some(b_bytes)) => {
            // 2. Bigger first.
            b_bytes
                .cmp(&a_bytes)
                // 3. At equal bytes, "at least this much" outranks
                //    "exactly this much". `true > false`, and we want
                //    `true` first, so the comparison is reversed.
                .then_with(|| {
                    b.reclaimable_bytes_is_lower_bound
                        .cmp(&a.reclaimable_bytes_is_lower_bound)
                })
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        // Two unmeasured candidates have nothing to rank by; fall through
        // to the deterministic tie-break.
        (None, None) => Ordering::Equal,
    }
    // 4. Determinism.
    .then_with(|| a.resource_id.cmp(b.resource_id))
}

/// Sort an assembled `detect` report's candidates into the canonical order.
///
/// Uses a stable sort even though [`compare`] is already total: if a future
/// change ever makes two entries compare equal, a stable sort degrades to
/// "input order" rather than to "arbitrary", which is the less surprising
/// failure.
pub fn sort_detect_candidates(candidates: &mut [DetectCandidateReport]) {
    candidates.sort_by(|a, b| {
        compare(
            RankInputs {
                reclaimable_bytes: a.reclaimable_bytes,
                reclaimable_bytes_is_lower_bound: a.reclaimable_bytes_is_lower_bound,
                resource_id: &a.resource_id,
            },
            RankInputs {
                reclaimable_bytes: b.reclaimable_bytes,
                reclaimable_bytes_is_lower_bound: b.reclaimable_bytes_is_lower_bound,
                resource_id: &b.resource_id,
            },
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(bytes: Option<u64>, lower_bound: bool, id: &str) -> RankInputs<'_> {
        RankInputs {
            reclaimable_bytes: bytes,
            reclaimable_bytes_is_lower_bound: lower_bound,
            resource_id: id,
        }
    }

    /// Order a list of `(bytes, is_lower_bound, resource_id)` and return the
    /// resulting ids, which is what every assertion below is really about.
    fn ordered_ids<'a>(mut items: Vec<RankInputs<'a>>) -> Vec<&'a str> {
        items.sort_by(|a, b| compare(*a, *b));
        items.into_iter().map(|i| i.resource_id).collect()
    }

    #[test]
    fn largest_reclaimable_size_comes_first() {
        // The actual reported defect: registration order put a tiny cache
        // above a huge build directory.
        let ids = ordered_ids(vec![
            inputs(Some(2 * 1024 * 1024), false, "npm_cache"),
            inputs(Some(40 * 1024 * 1024 * 1024), false, "cargo_target"),
            inputs(Some(512 * 1024 * 1024), false, "pip_cache"),
        ]);
        assert_eq!(ids, vec!["cargo_target", "pip_cache", "npm_cache"]);
    }

    #[test]
    fn unmeasured_candidates_sort_last_not_first() {
        // AC #6: a candidate we could not measure must never displace the
        // largest thing we could.
        let ids = ordered_ids(vec![
            inputs(None, false, "aaa_unmeasured"),
            inputs(Some(1), false, "zzz_one_byte"),
        ]);
        assert_eq!(
            ids,
            vec!["zzz_one_byte", "aaa_unmeasured"],
            "an unknown size must not outrank even a single measured byte"
        );
    }

    #[test]
    fn a_measured_zero_still_outranks_an_unmeasured_size() {
        // Zero is a fact; unknown is not.
        let ids = ordered_ids(vec![
            inputs(None, false, "unmeasured"),
            inputs(Some(0), false, "measured_empty"),
        ]);
        assert_eq!(ids, vec!["measured_empty", "unmeasured"]);
    }

    #[test]
    fn a_lower_bound_outranks_an_exact_figure_of_the_same_size() {
        // "≥ 5 GB" is at least as much as exactly 5 GB.
        let five_gb = 5 * 1024 * 1024 * 1024;
        let ids = ordered_ids(vec![
            inputs(Some(five_gb), false, "zzz_exact"),
            inputs(Some(five_gb), true, "aaa_at_least"),
        ]);
        assert_eq!(ids, vec!["aaa_at_least", "zzz_exact"]);

        // ...and the reverse input order gives the same answer, i.e. this is
        // a real comparison and not an accident of input order.
        let ids = ordered_ids(vec![
            inputs(Some(five_gb), true, "aaa_at_least"),
            inputs(Some(five_gb), false, "zzz_exact"),
        ]);
        assert_eq!(ids, vec!["aaa_at_least", "zzz_exact"]);
    }

    #[test]
    fn size_beats_the_lower_bound_tie_break() {
        // The lower-bound rule is only a tie-break. A larger exact figure
        // must still outrank a smaller lower bound — otherwise "≥ 1 MB"
        // would leapfrog an exact 30 GB.
        let ids = ordered_ids(vec![
            inputs(Some(1024 * 1024), true, "at_least_one_mb"),
            inputs(Some(30 * 1024 * 1024 * 1024), false, "exactly_thirty_gb"),
        ]);
        assert_eq!(ids, vec!["exactly_thirty_gb", "at_least_one_mb"]);
    }

    #[test]
    fn identical_sizes_and_flags_fall_back_to_path_order() {
        let ids = ordered_ids(vec![
            inputs(Some(100), false, "/b/second"),
            inputs(Some(100), false, "/c/third"),
            inputs(Some(100), false, "/a/first"),
        ]);
        assert_eq!(ids, vec!["/a/first", "/b/second", "/c/third"]);
    }

    #[test]
    fn unmeasured_candidates_are_ordered_among_themselves_deterministically() {
        let ids = ordered_ids(vec![
            inputs(None, false, "/z/last"),
            inputs(None, false, "/a/first"),
            inputs(None, false, "/m/middle"),
        ]);
        assert_eq!(ids, vec!["/a/first", "/m/middle", "/z/last"]);
    }

    #[test]
    fn the_order_is_stable_across_every_input_permutation() {
        // AC #1 is a determinism claim, so assert determinism directly
        // rather than asserting one happy-path order: every permutation of
        // the same six candidates must produce byte-identical output.
        let items = vec![
            inputs(Some(500), false, "/e"),
            inputs(Some(500), true, "/d"),
            inputs(None, false, "/f"),
            inputs(Some(9_000), false, "/a"),
            inputs(None, false, "/b"),
            inputs(Some(500), false, "/c"),
        ];
        let expected = vec!["/a", "/d", "/c", "/e", "/b", "/f"];

        // All 6! = 720 orderings, so this cannot pass by accident on the one
        // arrangement the test was written against.
        let permutations = all_permutations(&items);
        assert_eq!(permutations.len(), 720, "not actually exhaustive");
        for permutation in permutations {
            assert_eq!(
                ordered_ids(permutation),
                expected,
                "ordering depended on input order"
            );
        }
    }

    /// Every permutation of `items`, by Heap's algorithm. Written out rather
    /// than pulled in as a dependency because it is six lines and this is
    /// the only place that needs it.
    fn all_permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        let n = items.len();
        let mut current = items.to_vec();
        let mut out = vec![current.clone()];
        let mut counters = vec![0usize; n];
        let mut i = 0;
        while i < n {
            if counters[i] < i {
                current.swap(if i % 2 == 0 { 0 } else { counters[i] }, i);
                out.push(current.clone());
                counters[i] += 1;
                i = 0;
            } else {
                counters[i] = 0;
                i += 1;
            }
        }
        out
    }

    #[test]
    fn comparison_is_antisymmetric_for_distinct_candidates() {
        let a = inputs(Some(10), false, "/a");
        let b = inputs(Some(10), false, "/b");
        assert_eq!(compare(a, b), Ordering::Less);
        assert_eq!(compare(b, a), Ordering::Greater);
        assert_eq!(compare(a, a), Ordering::Equal);
    }

    #[test]
    fn policy_label_is_not_an_input_to_the_ordering() {
        // Structural, not behavioural: `RankInputs` has exactly three
        // fields and none of them is a policy verdict. If someone adds one,
        // this destructuring stops compiling, which is the point.
        let RankInputs {
            reclaimable_bytes: _,
            reclaimable_bytes_is_lower_bound: _,
            resource_id: _,
        } = inputs(Some(1), false, "/x");
    }
}
