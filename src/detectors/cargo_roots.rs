//! Configured extra Cargo target roots (HORO-2358).
//!
//! `CargoDetector` finds targets from `--project-root` workspaces. Build
//! output that lives outside every workspace — per-ticket targets under
//! `~/.cargo`, `/private/tmp/*-target`, a CI-style qualification directory —
//! is invisible to it, and on a developer machine those directories are often
//! the largest regenerable consumers. This module lets a user name them.
//!
//! The list lives in `~/Library/Application Support/Glomeris/
//! cargo-target-roots.conf`, one glob per line, `#` comments and blank lines
//! ignored. **It is empty by default**: a missing file means no extra roots,
//! so an upgrade changes nothing until the user opts in.
//!
//! Boundary (ADR-0001 §3, monitor/recommend only): a directory found here is
//! a *candidate with unresolved ownership*. It carries no project root, so it
//! classifies ASK with `TargetOwnershipUnknown` and `cargo clean` refuses to
//! run against it. Nothing here can raise a candidate toward `AUTO_SAFE`, and
//! discovery reads directory names and a few marker files — it never runs
//! `cargo`.
//!
//! Glob syntax is deliberately small: absolute paths or `~/`-relative paths,
//! `*` inside a path segment, nothing else. `**`, `?`, `[..]` and `..` are
//! rejected rather than half-supported, because each of them multiplies the
//! number of directories a scan would touch.

use std::path::{Path, PathBuf};

/// Directory (under the home directory) holding Glomeris's per-user files.
const CONFIG_DIR: [&str; 3] = ["Library", "Application Support", "Glomeris"];
const CONFIG_FILE: &str = "cargo-target-roots.conf";

/// Patterns read from the config file. More than this is a configuration
/// mistake, not a workload to honour.
pub const MAX_PATTERNS: usize = 64;
/// Directories a single expansion may return, across all patterns.
pub const MAX_MATCHES: usize = 512;
/// Directory entries read from any one directory while expanding a wildcard.
/// Bounds the cost of `~/.cargo/*` on a directory with a huge fan-out.
pub const MAX_ENTRIES_PER_DIR: usize = 20_000;
/// Directory entries read across ONE whole [`expand_globs`] call, however many
/// patterns and wildcard levels it has. The per-directory cap alone lets
/// `~/*/*/*` fan out into millions of reads.
pub const MAX_TOTAL_ENTRIES: usize = 100_000;
/// Path segments per pattern that may contain `*`.
pub const MAX_WILDCARD_SEGMENTS: usize = 3;
/// The config file is a short list of globs; anything larger is not one.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Where the user's list of extra target-root globs is read from.
pub fn config_path(home: &Path) -> PathBuf {
    let mut path = home.to_path_buf();
    for part in CONFIG_DIR {
        path.push(part);
    }
    path.push(CONFIG_FILE);
    path
}

/// Reads the configured globs. A missing or unreadable file yields no globs:
/// this list only ever *adds* candidates, so failing to read it can make
/// Glomeris see less, never more.
pub fn load_globs(home: &Path) -> Vec<String> {
    let path = config_path(home);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_CONFIG_BYTES => {}
        _ => return Vec::new(),
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_globs(&contents),
        Err(_) => Vec::new(),
    }
}

/// Parses the file body. Invalid patterns are dropped here (see
/// [`validate_pattern`]) rather than reported later, so a typo costs the user
/// one missing candidate and never a surprising scan.
pub fn parse_globs(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| validate_pattern(line))
        .map(str::to_string)
        .take(MAX_PATTERNS)
        .collect()
}

/// Whether `pattern` is inside the supported glob subset.
pub fn validate_pattern(pattern: &str) -> bool {
    let rooted = pattern.starts_with('/') || pattern.starts_with("~/");
    if !rooted || pattern.contains(['?', '[', ']', '{', '}', '\0']) || pattern.contains("**") {
        return false;
    }
    if pattern.split('/').any(|segment| segment == "..") {
        return false;
    }
    pattern
        .split('/')
        .filter(|segment| segment.contains('*'))
        .count()
        <= MAX_WILDCARD_SEGMENTS
}

/// Expands every pattern to existing directories, canonicalized and
/// de-duplicated, capped at [`MAX_MATCHES`].
pub fn expand_globs(patterns: &[String], home: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut budget = MAX_TOTAL_ENTRIES;
    for pattern in patterns {
        if found.len() >= MAX_MATCHES {
            break;
        }
        if !validate_pattern(pattern) {
            continue;
        }
        let absolute = match pattern.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(pattern),
        };
        let mut candidates = vec![PathBuf::from("/")];
        for segment in absolute.components().skip(1) {
            let segment = segment.as_os_str().to_string_lossy().into_owned();
            let mut next = Vec::new();
            for base in &candidates {
                if segment.contains('*') {
                    next.extend(matching_children(base, &segment, &mut budget));
                } else {
                    let joined = base.join(&segment);
                    if joined.is_dir() {
                        next.push(joined);
                    }
                }
                if next.len() >= MAX_MATCHES {
                    break;
                }
            }
            candidates = next;
            if candidates.is_empty() {
                break;
            }
        }
        for candidate in candidates {
            if found.len() >= MAX_MATCHES {
                break;
            }
            if let Ok(canonical) = candidate.canonicalize() {
                if canonical.is_dir() && !found.contains(&canonical) {
                    found.push(canonical);
                }
            }
        }
    }
    found
}

/// Children of `base` whose name matches `pattern`. Reads at most
/// [`MAX_ENTRIES_PER_DIR`] entries, in the directory's own (unspecified)
/// order, and charges every entry read against `budget`; an exhausted budget
/// yields no further matches, so a capped expansion can miss entries.
fn matching_children(base: &Path, pattern: &str, budget: &mut usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let take = MAX_ENTRIES_PER_DIR.min(*budget);
    let mut read = 0usize;
    let mut matches: Vec<PathBuf> = entries
        .take(take)
        .inspect(|_| read += 1)
        .filter_map(Result::ok)
        .filter(|entry| wildcard_match(pattern, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    *budget = budget.saturating_sub(read);
    // Sorted so the matches that survive the cap below are stable for the
    // entries that were read; which entries were read is up to the OS.
    matches.sort();
    matches.truncate(MAX_MATCHES);
    matches
}

/// `*` matches any run of characters (including none) within one segment.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let first = parts[0];
    let last = parts[parts.len() - 1];
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    true
}

/// Whether `dir` looks like Cargo build output rather than something else a
/// glob happened to match. Requires one of Cargo's own markers, and refuses a
/// directory that holds a manifest or Rust source: a misconfigured glob must
/// not turn a source tree into a candidate.
pub fn looks_like_cargo_target(dir: &Path) -> bool {
    if dir.join("Cargo.toml").exists() || dir.join("src").is_dir() {
        return false;
    }
    dir.join("CACHEDIR.TAG").is_file()
        || dir.join(".rustc_info.json").is_file()
        || dir.join("debug").is_dir()
        || dir.join("release").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-cargo-roots-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn wildcard_matches_prefix_suffix_and_middle() {
        assert!(wildcard_match("private-target-*", "private-target-abc"));
        assert!(wildcard_match("*-target", "ticket-target"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("private-target-*", "qual-target-abc"));
        assert!(!wildcard_match("a*b", "ab-extra"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exact2"));
    }

    #[test]
    fn only_the_small_glob_subset_is_accepted() {
        assert!(validate_pattern("~/.cargo/private-target-*"));
        assert!(validate_pattern("/private/tmp/*-target"));
        for rejected in [
            "relative/*",
            "~/**/target",
            "~/.cargo/../x",
            "~/a?b",
            "~/[ab]",
            "~/{a,b}",
        ] {
            assert!(!validate_pattern(rejected), "{rejected} must be rejected");
        }
    }

    #[test]
    fn parse_skips_comments_blanks_and_invalid_lines() {
        let body = "# per-ticket targets\n\n~/.cargo/private-target-*\nnot-absolute\n  /private/tmp/*-target  \n";
        assert_eq!(
            parse_globs(body),
            vec!["~/.cargo/private-target-*", "/private/tmp/*-target"]
        );
    }

    #[test]
    fn parse_caps_the_pattern_count() {
        let body: String = (0..MAX_PATTERNS + 10)
            .map(|i| format!("/tmp/x{i}\n"))
            .collect();
        assert_eq!(parse_globs(&body).len(), MAX_PATTERNS);
    }

    #[test]
    fn missing_config_file_means_no_roots() {
        let home = temp_dir("nofile");
        assert!(load_globs(&home).is_empty());
    }

    #[test]
    fn config_file_is_read_from_the_application_support_directory() {
        let home = temp_dir("cfg");
        let path = config_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "~/.cargo/qual-*\n").unwrap();
        assert_eq!(load_globs(&home), vec!["~/.cargo/qual-*"]);
    }

    #[test]
    fn expands_each_root_family_under_a_fixture_home() {
        let home = temp_dir("families");
        for dir in [
            ".cargo/qual-aaasm-6262-a",
            ".cargo/private-target-x",
            ".cargo/pkg-repro-target-y",
            ".cargo/isolated-targets/horo-1399",
            ".cargo/nda-targets/one",
            ".cargo/nda-targets/two",
            "AI-agent-assembly/private-target-z",
        ] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        // A file and a non-matching sibling must not match.
        fs::write(home.join(".cargo/private-target-file"), b"x").unwrap();
        fs::create_dir_all(home.join(".cargo/registry")).unwrap();

        let patterns: Vec<String> = [
            "~/.cargo/qual-*",
            "~/.cargo/private-target-*",
            "~/.cargo/pkg-repro-target-*",
            "~/.cargo/isolated-targets/*",
            "~/.cargo/nda-targets/*",
            "~/AI-agent-assembly/private-target-*",
        ]
        .map(str::to_string)
        .to_vec();
        let found = expand_globs(&patterns, &home);
        let rel: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&home)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(rel.len(), 7, "{rel:?}");
        assert!(rel.contains(&".cargo/nda-targets/two".to_string()));
        assert!(!rel
            .iter()
            .any(|p| p.contains("registry") || p.ends_with("file")));
    }

    #[test]
    fn overlapping_patterns_yield_each_directory_once() {
        let home = temp_dir("dedup");
        fs::create_dir_all(home.join("t/a")).unwrap();
        let patterns = vec![
            format!("{}/t/*", home.display()),
            format!("{}/t/a", home.display()),
        ];
        assert_eq!(expand_globs(&patterns, &home).len(), 1);
    }

    #[test]
    fn too_many_wildcard_segments_are_rejected() {
        assert!(validate_pattern("/a/*/b*/*c"));
        assert!(!validate_pattern("/a/*/*/*/*"));
    }

    #[test]
    fn an_oversized_config_file_is_ignored() {
        let home = temp_dir("bigcfg");
        let path = config_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = "# pad\n".repeat(20_000) + "~/.cargo/qual-*\n";
        assert!(body.len() as u64 > MAX_CONFIG_BYTES);
        fs::write(&path, body).unwrap();
        assert!(load_globs(&home).is_empty());
    }

    #[test]
    fn the_total_entry_budget_stops_a_wide_expansion() {
        let home = temp_dir("budget");
        for i in 0..40 {
            for j in 0..10 {
                fs::create_dir_all(home.join(format!("t/d{i:02}/e{j}"))).unwrap();
            }
        }
        // The first level costs 40 reads; a budget of 40 leaves nothing for the second.
        let mut budget = 40usize;
        let first = matching_children(&home.join("t"), "d*", &mut budget);
        assert_eq!(first.len(), 40);
        assert_eq!(budget, 0);
        assert!(matching_children(&first[0], "e*", &mut budget).is_empty());
    }

    #[test]
    fn expansion_is_capped() {
        let home = temp_dir("cap");
        for i in 0..MAX_MATCHES + 25 {
            fs::create_dir_all(home.join(format!("t/d{i:04}"))).unwrap();
        }
        let found = expand_globs(&[format!("{}/t/*", home.display())], &home);
        assert_eq!(found.len(), MAX_MATCHES);
    }

    #[test]
    fn marker_check_accepts_cargo_output_and_refuses_source_trees() {
        let root = temp_dir("markers");
        let tagged = root.join("tagged");
        fs::create_dir_all(&tagged).unwrap();
        fs::write(
            tagged.join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        assert!(looks_like_cargo_target(&tagged));

        let with_profile = root.join("profile");
        fs::create_dir_all(with_profile.join("debug")).unwrap();
        assert!(looks_like_cargo_target(&with_profile));

        let plain = root.join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert!(!looks_like_cargo_target(&plain));

        let source = root.join("source");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(source.join("CACHEDIR.TAG"), b"x").unwrap();
        assert!(
            !looks_like_cargo_target(&source),
            "a source tree is never a target"
        );

        let manifest = root.join("manifest");
        fs::create_dir_all(manifest.join("debug")).unwrap();
        fs::write(manifest.join("Cargo.toml"), b"[package]").unwrap();
        assert!(!looks_like_cargo_target(&manifest));
    }
}
