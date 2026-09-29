//! Opaque local aliases for the things history has to compare but must not
//! keep (HORO-1547).
//!
//! # What this is for
//!
//! The workflow baseline has to answer two questions across time:
//!
//! - *Is this the same repository I saw last week?* — so observations can be
//!   grouped per repository instead of averaged over the whole machine.
//! - *Is this the same branch the single checkout was on last time?* — which is
//!   the only thing that distinguishes `serial_single_checkout` from
//!   `serial_multi_branch`. At any one moment a single checkout has exactly one
//!   branch, so no snapshot can tell those two shapes apart; only a comparison
//!   between observations can.
//!
//! Both are *equality* questions. Neither needs the path or the branch name, so
//! neither keeps one. HORO-1547 asks for aggregates or opaque local ids in
//! preference to raw absolute paths, and an equality token is the smallest thing
//! that answers an equality question.
//!
//! # What this is not
//!
//! **Not a secret, and not trying to be.** There is no salt, and adding one
//! would be theatre: the salt would have to live in the same file as the
//! aliases, so anyone who can read the file would have both — and anyone who
//! can read the file can also just look at the user's repositories. Hiding
//! branch names from that reader is not a threat model that exists.
//!
//! What the alias genuinely buys is narrower and worth having: the history file
//! **contains no path and no branch name at all**, so it cannot leak one into a
//! bug report, a backup, a support attachment or a model payload by accident.
//! A value that is not there cannot be forwarded by mistake. That is the whole
//! claim, and `tests/workflow_history_holds_no_identity.rs` is what holds it.
//!
//! **Not a fingerprint for revalidation.** [`crate::evidence::model`]'s
//! fingerprints decide whether a deletion may proceed and are compared under
//! policy. An alias decides nothing; it groups explanatory observations. Keeping
//! the two vocabularies apart is deliberate — a type named "fingerprint" here
//! would invite exactly the reuse that must not happen.
//!
//! # Stability
//!
//! FNV-1a, written out rather than taken from [`std::hash`], because
//! `DefaultHasher`'s output is explicitly not stable across Rust releases and
//! these values are persisted. A toolchain upgrade that changed every stored
//! alias would read as "every repository is new and every branch just changed",
//! which is a silent reclassification rather than an error anybody sees.
//!
//! # Collisions
//!
//! Two distinct names sharing a 64-bit FNV-1a value would read as "unchanged".
//! Over the handful of short strings one machine's worktrees produce that is not
//! a risk worth engineering against, and the consequence is bounded: one
//! branch switch not counted, in a summary that already refuses to claim a
//! pattern from a small number of observations.

use std::path::Path;

/// An equality token for a repository or a branch. Carries no recoverable
/// information about what it aliases — see the module docs for what that does
/// and does not buy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalAlias(u64);

impl LocalAlias {
    /// Aliases an arbitrary byte string.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(fnv1a(bytes))
    }

    /// Aliases a name — a branch, or anything else compared as a string.
    pub fn of_name(name: &str) -> Self {
        Self::of_bytes(name.as_bytes())
    }

    /// Aliases a path by its bytes.
    ///
    /// Deliberately the whole path and not its final component: two worktrees
    /// of unrelated repositories are routinely both called `main`, and a
    /// baseline that merged them would report parallel development to someone
    /// who does none.
    ///
    /// No canonicalisation. Doing it here would touch the filesystem from a
    /// pure function and would fail for a path that has since been deleted —
    /// and a deleted worktree is exactly the case history exists to remember.
    /// Callers pass the same paths the graph was built from, which come from
    /// one `git` enumeration per run and are therefore already consistent
    /// within an observation.
    pub fn of_path(path: &Path) -> Self {
        Self::of_bytes(path.as_os_str().as_encoded_bytes())
    }

    /// The stored representation. `u64` rather than a formatted string so the
    /// serialized form cannot be mistaken for something with structure worth
    /// parsing.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Rebuilds an alias read back from the store.
    pub fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// 2^40 + 2^8 + 0xb3. Grouped from the right in full width, because the usual
/// `0x100000001b3` form is eleven digits and grouping eleven digits in fours
/// from the left silently yields a different number — which is precisely the
/// mistake `the_alias_of_a_known_string_is_a_fixed_number` caught here.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The one property the baseline actually rests on.
    #[test]
    fn the_same_name_aliases_the_same_way_and_a_different_one_does_not() {
        assert_eq!(LocalAlias::of_name("main"), LocalAlias::of_name("main"));
        assert_ne!(
            LocalAlias::of_name("main"),
            LocalAlias::of_name("feature/x"),
            "two branches that alias alike would read as a checkout that never switched"
        );
    }

    /// Pinned against a literal, because the whole point of writing FNV-1a out
    /// by hand is that the value is stable across toolchains. A refactor that
    /// changed the constants or the byte order would otherwise pass every
    /// behavioural test in this file while invalidating every alias already on
    /// a user's disk.
    #[test]
    fn the_alias_of_a_known_string_is_a_fixed_number() {
        // FNV-1a/64 of "main", computed independently of this implementation.
        assert_eq!(LocalAlias::of_name("main").as_u64(), 0x1f59_62a2_ce98_03c8);
        // A second input, because the first six bytes of a wrong-constant
        // result can still agree: the mistake this test actually caught left
        // the low half of "main" intact and only moved the top two bytes.
        assert_eq!(LocalAlias::of_name("trunk").as_u64(), 0x8c28_aca7_272b_5e3d);
        // The empty input must be the offset basis, not zero — a common
        // off-by-one when FNV is reimplemented.
        assert_eq!(LocalAlias::of_name("").as_u64(), FNV_OFFSET_BASIS);
    }

    #[test]
    fn a_path_aliases_by_its_whole_value_not_its_last_component() {
        let one = LocalAlias::of_path(&PathBuf::from("/a/project-one/main"));
        let two = LocalAlias::of_path(&PathBuf::from("/a/project-two/main"));
        assert_ne!(
            one, two,
            "two unrelated repositories both checked out on `main` would be merged into one, \
             and someone who develops serially would be shown parallel development"
        );
    }

    #[test]
    fn an_alias_survives_a_round_trip_through_its_stored_form() {
        let alias = LocalAlias::of_name("v0.0.1/HORO-1547/feat/workflow_profile");
        assert_eq!(LocalAlias::from_u64(alias.as_u64()), alias);
    }

    /// Not a security claim — see the module docs. It is the claim that the
    /// stored number does not simply contain the bytes it stands for, which is
    /// what would make a "no paths in the file" assertion vacuous.
    #[test]
    fn the_stored_number_does_not_contain_the_name_it_stands_for() {
        let name = "feature/secret-project";
        let stored = LocalAlias::of_name(name).as_u64().to_string();
        for window in name.as_bytes().windows(3) {
            let fragment = String::from_utf8_lossy(window);
            assert!(
                !stored.contains(fragment.as_ref()),
                "the stored alias {stored} spells out `{fragment}` from the name it aliases"
            );
        }
    }
}
