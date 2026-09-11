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

#[test]
fn zero_k_discards_everything() {
    let mut topk = TopKCandidates::new(0);
    topk.offer(ScanCandidate::new(PathBuf::from("/fixture/a"), 100, 1));
    assert!(topk.is_empty());
    assert_eq!(topk.into_sorted_vec().len(), 0);
}
