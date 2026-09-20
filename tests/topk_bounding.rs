//! Structural test: the bounded top-K aggregator never grows past `k`,
//! regardless of how many candidates are offered.

use std::path::PathBuf;

use glomeris::scanner::{ScanCandidate, TopKCandidates};

#[test]
fn heap_never_exceeds_k_across_thousands_of_offers() {
    const K: usize = 10;
    const TOTAL_OFFERED: u64 = 5_000;

    let mut topk = TopKCandidates::new(K);

    for i in 0..TOTAL_OFFERED {
        let candidate = ScanCandidate::new(PathBuf::from(format!("/fixture/file-{i}")), i, 1);
        topk.offer(candidate);

        // Structural assertion: the aggregation structure itself never
        // exceeds K, not just the final drained output.
        assert!(topk.len() <= K, "topk grew past K after {i} offers");
    }

    assert_eq!(topk.len(), K);

    let result = topk.into_sorted_vec();
    assert_eq!(result.len(), K);
    // Sizes were 0..TOTAL_OFFERED, so the top K are the K largest values,
    // sorted descending.
    let expected: Vec<u64> = (TOTAL_OFFERED - K as u64..TOTAL_OFFERED).rev().collect();
    let actual: Vec<u64> = result.iter().map(|c| c.logical_size_bytes).collect();
    assert_eq!(actual, expected);
}

/// HORO-1307 AC 1: two scans of an unchanged tree must return the same
/// order. Before the path tie-break, same-size candidates came out in
/// `BinaryHeap` drain order, which depends on the push/pop sequence — so
/// offering the identical set in a different sequence could produce a
/// different list, and any UI built on it would appear to shuffle for no
/// reason.
#[test]
fn equal_sized_candidates_come_out_in_path_order_whatever_order_they_arrived() {
    let paths = ["/fixture/d", "/fixture/b", "/fixture/a", "/fixture/c"];
    const SAME_SIZE: u64 = 4_096;

    // Offer the same four candidates in every rotation of the input, which
    // is enough to change the heap's internal layout.
    for rotation in 0..paths.len() {
        let mut topk = TopKCandidates::new(paths.len());
        for offset in 0..paths.len() {
            let path = paths[(offset + rotation) % paths.len()];
            topk.offer(ScanCandidate::new(PathBuf::from(path), SAME_SIZE, 1));
        }

        let ordered: Vec<String> = topk
            .into_sorted_vec()
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            ordered,
            vec!["/fixture/a", "/fixture/b", "/fixture/c", "/fixture/d"],
            "order depended on the sequence candidates were offered in (rotation {rotation})"
        );
    }
}

/// Size still dominates: the tie-break must only apply within a size group.
#[test]
fn the_path_tie_break_never_outranks_a_larger_candidate() {
    let mut topk = TopKCandidates::new(3);
    topk.offer(ScanCandidate::new(PathBuf::from("/zzz/huge"), 9_000, 1));
    topk.offer(ScanCandidate::new(PathBuf::from("/aaa/small"), 10, 1));
    topk.offer(ScanCandidate::new(PathBuf::from("/bbb/medium"), 500, 1));

    let ordered: Vec<String> = topk
        .into_sorted_vec()
        .iter()
        .map(|c| c.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(ordered, vec!["/zzz/huge", "/bbb/medium", "/aaa/small"]);
}

#[test]
fn zero_k_discards_everything() {
    let mut topk = TopKCandidates::new(0);
    topk.offer(ScanCandidate::new(PathBuf::from("/fixture/a"), 100, 1));
    assert!(topk.is_empty());
    assert_eq!(topk.into_sorted_vec().len(), 0);
}
