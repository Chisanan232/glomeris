//! Nested-candidate containment (HORO-2360).
//!
//! Two discovered candidates can overlap on disk: a directory and one of its
//! own children are both reported, and a naive sum of their sizes counts the
//! child's bytes twice. This module only answers "which candidate, if any,
//! encloses this one"; the reporting layer uses the answer to avoid summing a
//! contained candidate's bytes into a total that already includes its
//! enclosing candidate's.
//!
//! It is pure (no I/O) and report-only: it never feeds classification,
//! planning or execution, and a miss can only leave today's over-count, never
//! hide a candidate (ADR-0001 §20).

use std::path::Path;

use super::model::{Evidence, ResourceLocator};

/// For each entry, the index of the nearest *enclosing* entry, if any.
///
/// Entry `d` is enclosed by entry `a` when `d`'s path is strictly under `a`'s
/// (component-wise, so `/a/target2` is not under `/a/target`), or when the two
/// paths are equal and `a` comes earlier in the slice, which keeps equal paths
/// from enclosing each other in a cycle. `None` paths (tool-native resources
/// with no filesystem location) never enclose and are never enclosed.
///
/// `O(n log n)`: a sort puts every ancestor before its descendants, then one
/// pass keeps a stack of the current ancestor chain.
pub fn nearest_enclosing(paths: &[Option<&Path>]) -> Vec<Option<usize>> {
    let mut enclosing = vec![None; paths.len()];
    let mut order: Vec<usize> = (0..paths.len()).filter(|&i| paths[i].is_some()).collect();
    order.sort_by(|&a, &b| paths[a].cmp(&paths[b]).then(a.cmp(&b)));

    let mut stack: Vec<usize> = Vec::new();
    for idx in order {
        let path = paths[idx].expect("filtered to Some");
        while let Some(&top) = stack.last() {
            if path.starts_with(paths[top].expect("only Some paths are pushed")) {
                break;
            }
            stack.pop();
        }
        enclosing[idx] = stack.last().copied();
        stack.push(idx);
    }
    enclosing
}

/// [`nearest_enclosing`] over the filesystem locations of `evidence`.
pub fn enclosing_indices(evidence: &[&Evidence]) -> Vec<Option<usize>> {
    let paths: Vec<Option<&Path>> = evidence
        .iter()
        .map(|ev| match &ev.resource.locator {
            ResourceLocator::Path(path) => Some(path.as_path()),
            ResourceLocator::Tool { .. } => None,
        })
        .collect();
    nearest_enclosing(&paths)
}

/// For each evidence, the `resource_id` of the nearest other evidence whose
/// filesystem path encloses it (see [`nearest_enclosing`]).
pub fn enclosing_resource_ids<'a>(
    evidence: impl IntoIterator<Item = &'a Evidence>,
) -> Vec<Option<String>> {
    let evidence: Vec<&Evidence> = evidence.into_iter().collect();
    enclosing_indices(&evidence)
        .into_iter()
        .map(|enclosing| enclosing.map(|i| evidence[i].resource.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> Option<PathBuf> {
        Some(PathBuf::from(s))
    }

    fn run(paths: &[Option<PathBuf>]) -> Vec<Option<usize>> {
        let refs: Vec<Option<&Path>> = paths.iter().map(|p| p.as_deref()).collect();
        nearest_enclosing(&refs)
    }

    #[test]
    fn a_strict_ancestor_encloses_its_descendant() {
        assert_eq!(
            run(&[p("/x/targets"), p("/x/targets/one")]),
            vec![None, Some(0)]
        );
        assert_eq!(
            run(&[p("/x/targets/one"), p("/x/targets")]),
            vec![Some(1), None]
        );
    }

    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_enclosed() {
        assert_eq!(run(&[p("/a/target"), p("/a/target2")]), vec![None, None]);
        assert_eq!(
            run(&[p("/a/target"), p("/a/target2/x"), p("/a/target/y")]),
            vec![None, None, Some(0)]
        );
    }

    #[test]
    fn the_nearest_enclosing_entry_wins_in_a_three_level_chain() {
        assert_eq!(
            run(&[p("/r"), p("/r/a/b"), p("/r/a")]),
            vec![None, Some(2), Some(0)]
        );
    }

    #[test]
    fn equal_paths_enclose_by_order_and_never_in_a_cycle() {
        assert_eq!(run(&[p("/r/t"), p("/r/t")]), vec![None, Some(0)]);
        assert_eq!(
            run(&[p("/r/t"), p("/r/t"), p("/r/t")]),
            vec![None, Some(0), Some(1)]
        );
    }

    #[test]
    fn tool_resources_neither_enclose_nor_are_enclosed() {
        assert_eq!(
            run(&[None, p("/r"), None, p("/r/x")]),
            vec![None, None, None, Some(1)]
        );
    }

    #[test]
    fn unrelated_roots_do_not_enclose_each_other() {
        assert_eq!(
            run(&[p("/a"), p("/b"), p("/b/c")]),
            vec![None, None, Some(1)]
        );
    }
}
