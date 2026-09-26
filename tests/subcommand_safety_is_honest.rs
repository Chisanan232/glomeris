//! A command's safety claim is true of everything reachable through it
//! (HORO-1485).
//!
//! # The defect
//!
//! `glomeris daemon --help` printed "Read-only — changes nothing." above a
//! list containing `install` and `uninstall`. `install` writes a launch agent
//! plist and `launchctl load`s it; `uninstall` unloads and deletes it; `run`
//! records pressure history and a heartbeat under
//! `Library/Application Support/Glomeris`. Three of the four verbs write, and
//! the banner above them said none of them did. `glomeris autopilot --help`
//! had the mirror-image fault: one banner reading "Can delete data" over
//! `show`, which reads a config file and prints it.
//!
//! Neither was a wording problem. `Safety` was declared once per *command*
//! while the consequence varies per *verb*, so no wording could have been
//! true. The fix gives each verb its own declaration; these tests assert the
//! property that made the old declarations wrong, so that they cannot be
//! satisfied again by editing a string.
//!
//! # What each test grounds the property in
//!
//! The unit tests in `src/cli/help.rs` check the table against itself: that a
//! command's own label is the strongest of its verbs', and that a rendered
//! banner is true of every verb under it. Self-consistency is necessary and
//! not sufficient — a table that is internally coherent and describes verbs
//! the binary does not have, or omits verbs it does, is coherently wrong.
//! So the two tests here reach outside the table:
//!
//! - `declared_subcommands_are_exactly_the_verbs_the_binary_dispatches` and
//!   `every_verb_dispatch_site_belongs_to_a_command_that_declares_subcommands`
//!   read `src/main.rs`. "Reachable" means "the binary dispatches on it", and
//!   that is a fact about the dispatch arms, not about the help table. Same
//!   idiom, and the same reason for it, as
//!   `tests/shared_command_table.rs::help_mentions_every_subcommand_the_binary_actually_dispatches`.
//!
//! - `read_only_surfaces_leave_a_disposable_home_untouched` *runs* every
//!   surface declared `ReadOnly` and compares the filesystem before and
//!   after. "Changes nothing" is a claim about behaviour, and the only
//!   honest way to assert it is to observe it.
//!
//! # What is deliberately not executed, and why
//!
//! The mutating declarations are not verified by running them. `daemon
//! install` writes a plist into `LaunchAgents` and calls `launchctl load`,
//! and `daemon uninstall` unloads and removes it: with `HOME` redirected the
//! *file* would land in the temporary directory, but `launchctl` talks to the
//! real per-user domain, so the test would register or tear down a launch
//! agent on whatever machine ran it. `daemon run` never returns — it is the
//! monitor loop. `autopilot run` and the `Destructive` commands delete data
//! by design.
//!
//! So of the six mutating surfaces, exactly one is safely executable:
//! `autopilot enable` writes a single file under a `HOME`-derived path and
//! exits. It is used below as the positive control, which is what makes the
//! read-only half non-vacuous — it proves that a command which writes writes
//! *into the redirected `HOME`*, and that the comparison notices. Without it,
//! a binary that ignored `HOME` entirely would pass every assertion here
//! while writing to the real one.
//!
//! `tests/shared_command_table.rs` records why no test in this repository
//! probes subcommands by running them speculatively: an earlier version of
//! that file did, and `glomeris emergency` discarded its arguments, so the
//! probe performed three real emergency recovery runs. Nothing here runs a
//! surface that is not declared safe to run, and the one exception is named.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use glomeris::cli::help::{self, Safety};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn main_source() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("failed to read src/main.rs")
}

/// The handler `main` routes this command's remaining arguments to.
fn handler_name(command: &help::CommandSpec) -> String {
    format!("run_{}_command", command.name.replace('-', "_"))
}

/// The literal in a dispatch arm — `Some("install") =>` or `"show" =>` —
/// or `None` for any other line.
///
/// Flags are dispatched by the same shape (`"--json" =>`), so the caller
/// filters them out by their leading dash; a subcommand name is a bare verb,
/// which `a_subcommand_name_is_a_bare_verb` in `src/cli/help.rs` pins.
fn dispatch_arm_literal(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let after_some = trimmed.strip_prefix("Some(").unwrap_or(trimmed);
    let (literal, rest) = after_some.strip_prefix('"')?.split_once('"')?;
    let rest = rest.strip_prefix(')').unwrap_or(rest);
    rest.starts_with(" =>").then_some(literal)
}

/// Walks `source` a line at a time, reporting the name of the innermost
/// top-level `fn` each line belongs to.
///
/// Top-level only: a nested `fn` is indented, and every dispatcher in
/// `main.rs` is at column zero. `#[cfg]`-duplicated functions
/// (`run_status_command` has a macOS and a non-macOS definition) both report
/// under the same name, so a verb declared under either is seen.
fn lines_by_enclosing_fn(source: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut current = "";
    source.lines().filter_map(move |line| {
        let signature = line
            .strip_prefix("pub fn ")
            .or_else(|| line.strip_prefix("fn "));
        if let Some(signature) = signature {
            current = signature.split(['(', '<']).next().unwrap_or("");
            return None;
        }
        (!current.is_empty()).then_some((current, line))
    })
}

/// Sorted, deduplicated verbs `fn <handler>` dispatches on.
fn dispatched_verbs(source: &str, handler: &str) -> Vec<String> {
    let mut verbs: Vec<String> = lines_by_enclosing_fn(source)
        .filter(|(name, _)| *name == handler)
        .filter_map(|(_, line)| dispatch_arm_literal(line))
        .filter(|verb| !verb.starts_with('-'))
        .map(str::to_string)
        .collect();
    verbs.sort();
    verbs.dedup();
    verbs
}

/// AC 4, first half: the declarations describe the verbs the binary has.
///
/// Equality in both directions. A declared verb the binary does not dispatch
/// is a label nobody can reach; a dispatched verb with no declaration is a
/// consequence nobody is told about, which is how `daemon run` came to be
/// covered by a "changes nothing" banner while writing two files.
#[test]
fn declared_subcommands_are_exactly_the_verbs_the_binary_dispatches() {
    let source = main_source();
    let mut commands_with_verbs = 0usize;

    for command in help::COMMANDS {
        let handler = handler_name(command);
        let dispatched = dispatched_verbs(&source, &handler);

        let mut declared: Vec<String> = command
            .subcommands
            .iter()
            .map(|sub| sub.name.to_string())
            .collect();
        declared.sort();
        assert!(
            declared.windows(2).all(|pair| pair[0] != pair[1]),
            "'{}' declares the same subcommand twice: {declared:?}",
            command.name
        );

        assert_eq!(
            dispatched, declared,
            "'{}' declares subcommands {declared:?} but `{handler}` in src/main.rs \
             dispatches {dispatched:?}",
            command.name
        );

        if !dispatched.is_empty() {
            commands_with_verbs += 1;
        }
    }

    // A parse that silently found nothing would agree with every `&[]` in the
    // table and report a pass. `daemon`, `actions` and `autopilot` have verbs.
    assert!(
        commands_with_verbs >= 3,
        "found verbs for only {commands_with_verbs} command(s) — this test's parse of \
         src/main.rs has broken, and a broken parse here reads as a pass"
    );
}

/// AC 4, second half: a command cannot acquire verbs without declaring them.
///
/// The test above compares, per command, against the handler named after it.
/// That alone would miss a *new* command that dispatches verbs from somewhere
/// this test does not look — it would compare an empty declaration against an
/// empty parse and pass. So the dispatch sites themselves are enumerated:
/// reading a verb out of an argument list is spelled
/// `args.first().map(String::as_str)` throughout this CLI, and every
/// occurrence of it has to be accounted for.
#[test]
fn every_verb_dispatch_site_belongs_to_a_command_that_declares_subcommands() {
    let source = main_source();

    let sites: BTreeSet<&str> = lines_by_enclosing_fn(&source)
        .filter(|(_, line)| line.contains("args.first().map(String::as_str)"))
        .map(|(name, _)| name)
        .collect();

    let mut expected: BTreeSet<String> = help::COMMANDS
        .iter()
        .filter(|command| !command.subcommands.is_empty())
        .map(handler_name)
        .collect();
    // `main` dispatches the command names themselves, which
    // `tests/shared_command_table.rs` checks against the table.
    expected.insert("main".to_string());
    // `help` dispatches *topics*, listed in `help::HELP_TOPICS`; it is not in
    // `COMMANDS` and has no subcommands.
    expected.insert("run_help_command".to_string());

    let sites: BTreeSet<String> = sites.into_iter().map(str::to_string).collect();
    assert_eq!(
        sites, expected,
        "src/main.rs reads a subcommand verb somewhere unaccounted for. Every command \
         that dispatches verbs must declare them in help::COMMANDS, so that each one \
         carries its own safety label"
    );
}

// ---------------------------------------------------------------------------
// Observed behaviour: the read-only surfaces
// ---------------------------------------------------------------------------

fn make_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Every path under `root`, with file contents, for byte-for-byte comparison.
///
/// Directories map to `None` so that creating an empty one is a difference:
/// `Library/Application Support/Glomeris/` appearing is a write even before
/// anything is put in it. `symlink_metadata` so a symlink is compared as a
/// symlink rather than followed out of the tree.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => panic!("failed to read {}: {e}", dir.display()),
        };
        for entry in entries {
            let entry = entry.expect("failed to read a directory entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("walked outside the snapshot root")
                .to_path_buf();
            let meta = fs::symlink_metadata(&path).expect("failed to stat an entry");
            if meta.is_dir() {
                out.insert(relative, None);
                walk(&path, root, out);
            } else if meta.is_symlink() {
                let target = fs::read_link(&path).expect("failed to read a symlink");
                out.insert(
                    relative,
                    Some(target.as_os_str().as_encoded_bytes().to_vec()),
                );
            } else {
                out.insert(
                    relative,
                    Some(fs::read(&path).expect("failed to read a file")),
                );
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Runs the binary with `HOME` pointed at `home`.
///
/// `HOME` is the only thing redirected because it is the only thing these
/// commands resolve state through: every writer in `main.rs` and
/// `autopilot::store::default_envelope_path` builds its path from
/// `$HOME/Library/Application Support/Glomeris/`.
fn run_with_home(args: &[String], home: &Path) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Every surface the table declares `ReadOnly`, as the argv that exercises it.
///
/// Derived from `help::COMMANDS` rather than listed, so a command or verb that
/// becomes `ReadOnly` is covered by the assertion below the moment it is
/// declared — and one that has no invocation here fails rather than being
/// skipped.
fn read_only_surfaces() -> Vec<String> {
    let mut surfaces = Vec::new();
    for command in help::COMMANDS {
        if command.subcommands.is_empty() {
            if command.safety == Safety::ReadOnly {
                surfaces.push(command.name.to_string());
            }
            continue;
        }
        for sub in command.subcommands {
            if sub.safety == Safety::ReadOnly {
                surfaces.push(format!("{} {}", command.name, sub.name));
            }
        }
    }
    surfaces.sort();
    surfaces
}

/// How to invoke each read-only surface, including the `--json` variant where
/// one exists, since a different output path could take a different code path.
///
/// `explain` needs something to explain: it is given a path outside the
/// disposable `HOME`, so that a surface which turned out to write beside the
/// resource it was asked about would be visible as a change to the `HOME`
/// tree rather than hidden in it.
fn read_only_invocations(scratch: &Path) -> Vec<(String, Vec<String>)> {
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    vec![
        ("status".to_string(), argv(&["status"])),
        ("status".to_string(), argv(&["status", "--json"])),
        ("scan".to_string(), argv(&["scan"])),
        ("detect".to_string(), argv(&["detect"])),
        ("detect".to_string(), argv(&["detect", "--json"])),
        (
            "explain".to_string(),
            vec!["explain".to_string(), scratch.display().to_string()],
        ),
        ("history".to_string(), argv(&["history"])),
        ("actions list".to_string(), argv(&["actions", "list"])),
        (
            "actions list".to_string(),
            argv(&["actions", "list", "--json"]),
        ),
        ("actions history".to_string(), argv(&["actions", "history"])),
        (
            "actions history".to_string(),
            argv(&["actions", "history", "--json"]),
        ),
        ("autopilot show".to_string(), argv(&["autopilot", "show"])),
        (
            "autopilot show".to_string(),
            argv(&["autopilot", "show", "--json"]),
        ),
        ("daemon status".to_string(), argv(&["daemon", "status"])),
        (
            "daemon status".to_string(),
            argv(&["daemon", "status", "--json"]),
        ),
    ]
}

/// AC 2 and AC 3, observed: every surface labelled "Read-only — changes
/// nothing." leaves the tree it would write to byte-for-byte identical, and
/// `daemon status` is among them rather than having been relabelled away.
#[test]
fn read_only_surfaces_leave_a_disposable_home_untouched() {
    let scratch = make_temp_dir("horo1485-scratch");
    fs::write(scratch.join("a-file"), vec![0u8; 1024]).expect("write scratch file");

    let invocations = read_only_invocations(&scratch);

    let covered: BTreeSet<String> = invocations.iter().map(|(s, _)| s.clone()).collect();
    let required: BTreeSet<String> = read_only_surfaces().into_iter().collect();
    assert_eq!(
        covered, required,
        "the read-only surfaces this test runs are not the ones the table declares \
         read-only; a surface with no invocation here would be asserted about by nobody"
    );

    for (surface, args) in &invocations {
        let home = make_temp_dir("horo1485-home");
        let before = snapshot(&home);

        let output = run_with_home(args, &home);
        // Not asserted to succeed: `daemon status` exits non-zero off macOS,
        // and `explain` exits non-zero when nothing matches. What is under
        // test is that the process wrote nothing, whatever it concluded.
        assert!(
            output.status.code().is_some(),
            "`glomeris {}` was killed by a signal rather than exiting",
            args.join(" ")
        );

        let after = snapshot(&home);
        assert_eq!(
            before,
            after,
            "'{surface}' is declared read-only but `glomeris {}` changed its HOME \
             (exit {:?}). Paths before: {:?}; after: {:?}",
            args.join(" "),
            output.status.code(),
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
    }
}

/// The control that makes the test above mean something.
///
/// If the binary resolved its state directory from anything other than `HOME`
/// — a hardcoded path, a cached value, `NSHomeDirectory()` — then every
/// assertion above would pass while the commands wrote to the real home
/// directory, and this suite would be certifying the opposite of what it
/// claims. So a surface that is *declared* to write is run the same way, and
/// the disposable `HOME` is required to change.
///
/// `autopilot enable` is the only mutating surface that can be run safely
/// here: it writes one file under the `HOME`-derived state directory and
/// returns. Its declaration, `WritesOwnState`, is the thing being confirmed —
/// it writes, and what it writes is Glomeris's own envelope and nothing else.
#[test]
fn a_surface_declared_to_write_does_write_into_the_disposable_home() {
    let home = make_temp_dir("horo1485-control-home");
    let before = snapshot(&home);

    let args = ["autopilot", "enable", "--kinds", "cargo_target_dir"]
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<String>>();
    let output = run_with_home(&args, &home);
    assert!(
        output.status.success(),
        "`glomeris autopilot enable --kinds cargo_target_dir` should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = snapshot(&home);
    assert_ne!(
        before, after,
        "`autopilot enable` is declared to write Glomeris's own state, but the \
         disposable HOME is unchanged — so either it wrote somewhere else (and every \
         read-only assertion in this file is vacuous), or it wrote nothing (and its \
         declaration is wrong)"
    );

    // And what it wrote is its own state, under its own directory.
    let written: Vec<&PathBuf> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    assert!(
        written.iter().all(
            |path| path.starts_with("Library/Application Support/Glomeris")
                || path == &&PathBuf::from("Library")
                || path == &&PathBuf::from("Library/Application Support")
        ),
        "`autopilot enable` wrote outside Glomeris's own state directory: {written:?}"
    );
    assert!(
        written.iter().any(|path| path.ends_with("autopilot.conf")),
        "`autopilot enable` did not write the envelope: {written:?}"
    );
}
