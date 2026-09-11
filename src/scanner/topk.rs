//! Bounded top-K aggregation of [`ScanCandidate`]s.
//!
//! Holds at most `k` candidates at any point in time, keyed by
//! `logical_size_bytes`. The structure never grows past `k` regardless of
//! how many candidates are offered — this is what lets the scanner walk
//! arbitrarily large trees in bounded memory instead of buffering the
//! full file list.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use super::candidate::ScanCandidate;

/// Wraps a [`ScanCandidate`] with a size-only ordering so it can live in a
/// [`BinaryHeap`]. The ordering is reversed relative to `logical_size_bytes`
/// so that `BinaryHeap` (a max-heap) behaves as a min-heap on size: the
/// item on top is always the *smallest* one currently held, which is
/// exactly the one to evict when a bigger candidate arrives.
struct HeapEntry(ScanCandidate);

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0.logical_size_bytes == other.0.logical_size_bytes
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.logical_size_bytes.cmp(&self.0.logical_size_bytes)
    }
}

/// A bounded min-heap holding the top `k` largest [`ScanCandidate`]s seen
/// so far. `len()` never exceeds `k`.
pub struct TopKCandidates {
    k: usize,
    heap: BinaryHeap<HeapEntry>,
}

impl TopKCandidates {
    /// Create an aggregator bounded to `k` candidates. `k == 0` is valid
    /// and simply discards every offered candidate.
    pub fn new(k: usize) -> Self {
        Self {
            k,
            heap: BinaryHeap::with_capacity(k.min(1024)),
        }
    }

    /// Offer a candidate for inclusion. Runs in O(log k) time and never
    /// grows the heap past `k` entries: below capacity the candidate is
    /// always kept, at capacity it replaces the current smallest entry
    /// only if it is bigger.
    pub fn offer(&mut self, candidate: ScanCandidate) {
        if self.k == 0 {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(HeapEntry(candidate));
            return;
        }
        if let Some(smallest) = self.heap.peek() {
            if candidate.logical_size_bytes > smallest.0.logical_size_bytes {
                self.heap.pop();
                self.heap.push(HeapEntry(candidate));
            }
        }
    }

    /// Current number of candidates held. Always `<= k`.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// True when no candidate has been retained.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Consume the aggregator, returning its candidates sorted by
    /// descending logical size.
    pub fn into_sorted_vec(self) -> Vec<ScanCandidate> {
        let mut v: Vec<ScanCandidate> = self.heap.into_iter().map(|e| e.0).collect();
        v.sort_by(|a, b| b.logical_size_bytes.cmp(&a.logical_size_bytes));
        v
    }
}
