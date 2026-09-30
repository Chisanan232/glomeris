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
//! # Whose writes the comparison is about (HORO-1556, HORO-1560)
//!
//! `detect` asks installed build tools where their caches are, by running
//! them: `npm config get cache`, `go env GOCACHE`, `pip cache dir`,
//! `uv cache dir`, `brew --cache`, `docker system df`. Several of those write
//! to `$HOME` as a side effect of being asked anything at all — observed on
//! the workstation this was written on, inside a `HOME` that was created
//! empty a moment earlier:
//!
//! * `go env` wrote a telemetry counter under
//!   `Library/Application Support/go/telemetry/local/`. `go help telemetry`
//!   documents the mode as settable only by `go telemetry off` — a user
//!   preference — and `GOTELEMETRY` as a *non-settable* `go env` variable.
//!   No environment Glomeris can construct suppresses it.
//! * `pip`, `uv` and `npm` each populated
//!   `Library/Caches/BytecodeAlliance.wasmtime/`, because on this machine
//!   they are `proto` shims and `proto` compiles its WASM plugins on first
//!   use. That is a property of the user's installation, not of the tool.
//! * `npm` created its own cache directory. It no longer writes
//!   `_logs/<timestamp>-debug-0.log` *inside the directory being measured* —
//!   that write was Glomeris's to fix, and the probe passes `--logs-max=0`
//!   (HORO-1556) — but the `mkdir` is still a write.
//!
//! So "changes nothing" was never true of `detect`, and no wording could have
//! made it true while the answer comes from running somebody else's program.
//! A version manager fronting one of those tools can go further and provision
//! a whole toolchain on first use. That is why `detect` and `explain` are
//! declared `Safety::ConsultsInstalledTools` rather than `Safety::ReadOnly`
//! (HORO-1560), and why the tests below are split along the same line instead
//! of being weakened until one claim covers both:
//!
//! * the surfaces still declared `ReadOnly` are held to byte-for-byte
//!   identity — with nothing on `PATH`, and again with the machine's real
//!   `PATH`. The second assertion is what the split buys: those surfaces
//!   start no other program at all, so nothing is left that could write, and
//!   the claim holds in the configuration a user actually runs. It is also
//!   what catches the regressions this file was written for — `settings show`
//!   writing out the defaults it just reported, `pressure show` opening an
//!   episode.
//! * the surfaces declared `ConsultsInstalledTools` are held to the claim
//!   their own label makes: no state Glomeris owns, and whatever a foreign
//!   tool left behind is *recorded* rather than asserted to be nothing.
//! * which surfaces start another program is not taken on trust either. Every
//!   surface in both sets is run against a `PATH` of recording stubs named
//!   after `glomeris::detectors::SPAWNED_PROGRAMS`, and each label is checked
//!   against what was observed to run. Declaring `detect` read-only again
//!   fails there, which is the property that makes the split more than a
//!   rename.
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
use glomeris::detectors::SPAWNED_PROGRAMS;

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

/// The same run, with a `PATH` holding no executables at all, so that no tool
/// a detector consults can start (HORO-1556).
///
/// Every detector that shells out does so by program name, so an empty `PATH`
/// makes each one report `ToolAbsent` — a real answer, and one that leaves
/// Glomeris as the only process that could have written anything. That is what
/// makes the byte-for-byte comparison below a statement about Glomeris rather
/// than about the machine's build tools.
fn run_with_home_and_no_tools(
    args: &[String],
    home: &Path,
    empty_path: &Path,
) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .env("PATH", empty_path)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Paths under `$HOME` that belong to Glomeris and to nothing else.
///
/// Used by the real-`PATH` half of the read-only claim, where a third-party
/// tool may legitimately have written its own files but Glomeris may not have
/// written any of its own. Every state file the binary owns lives under the
/// first of these — `history.tsv`, `actions.jsonl`, `heartbeat.json`, the
/// episode store, the settings file and the Autopilot envelope.
const GLOMERIS_OWNED_PREFIXES: &[&str] = &["Library/Application Support/Glomeris"];

/// Every surface the table declares with `safety`, named as `"command verb"`.
///
/// Derived from `help::COMMANDS` rather than listed, so a command or verb that
/// acquires one of these labels is covered by the assertions below the moment
/// it is declared — and one that has no invocation here fails rather than
/// being skipped.
fn surfaces_declared(safety: Safety) -> Vec<String> {
    let mut surfaces = Vec::new();
    for command in help::COMMANDS {
        if command.subcommands.is_empty() {
            if command.safety == safety {
                surfaces.push(command.name.to_string());
            }
            continue;
        }
        for sub in command.subcommands {
            if sub.safety == safety {
                surfaces.push(format!("{} {}", command.name, sub.name));
            }
        }
    }
    surfaces.sort();
    surfaces
}

/// The two labels that look alike to a reader skimming `--help` and are held
/// to different claims below (HORO-1560).
///
/// `ReadOnly` promises the whole filesystem is untouched, including by
/// anything Glomeris starts. `ConsultsInstalledTools` promises only that
/// Glomeris writes nothing *of its own*. Nothing else in the table may reach
/// the tests below: a surface labelled `Destructive` must not be run at all.
const INSPECTING_LABELS: &[Safety] = &[Safety::ReadOnly, Safety::ConsultsInstalledTools];

/// Which of [`INSPECTING_LABELS`] a surface named by [`surfaces_declared`]
/// carries.
fn declared_safety(surface: &str) -> Safety {
    for label in INSPECTING_LABELS {
        if surfaces_declared(*label).iter().any(|s| s == surface) {
            return *label;
        }
    }
    panic!("'{surface}' is not declared with any label this test may run");
}

/// How to invoke each inspecting surface, including the `--json` variant where
/// one exists, since a different output path could take a different code path.
///
/// `explain` needs something to explain: it is given a path outside the
/// disposable `HOME`, so that a surface which turned out to write beside the
/// resource it was asked about would be visible as a change to the `HOME`
/// tree rather than hidden in it.
fn inspect_invocations(scratch: &Path) -> Vec<(String, Vec<String>)> {
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
        // Worth its own scrutiny: `settings show` reads a file that may not
        // exist yet and answers with the built-in defaults. The tempting
        // implementation writes those defaults out so the next read is simple,
        // which would make reading your preferences create them (HORO-1507).
        ("settings show".to_string(), argv(&["settings", "show"])),
        (
            "settings show".to_string(),
            argv(&["settings", "show", "--json"]),
        ),
        // Worth its own scrutiny for a different reason: `pressure show` is
        // asked "is a notification owed?" while standing next to an
        // `EpisodeTracker` that could answer by observing the disk — and
        // observing opens episodes. If it did, the menu-bar app would become a
        // second place where pressure policy is decided, notifications would
        // appear on machines whose monitor was never installed, and a verb
        // named `show` would have side effects. The evidence that it does not
        // is that the episode state file is absent afterwards (HORO-1508).
        ("pressure show".to_string(), argv(&["pressure", "show"])),
        (
            "pressure show".to_string(),
            argv(&["pressure", "show", "--json"]),
        ),
        // Worth its own scrutiny for a third reason: `external-context` reports
        // on providers that reach the network, and the honest-looking way to
        // report whether one works is to try it. Running it inside a disposable
        // HOME with nothing on PATH proves it writes nothing; that it also
        // *asks* nothing is proven by
        // `scripts/check-external-context-is-read-only.sh` and by the command
        // taking no transport at all.
        ("external-context".to_string(), argv(&["external-context"])),
        (
            "external-context".to_string(),
            argv(&["external-context", "--json"]),
        ),
        // Worth its own scrutiny for a fourth reason: `workflow-profile show`
        // reads a baseline file that usually does not exist yet, standing next
        // to the recorder that would create one. A `show` that took an
        // observation "so there is something to show" would be the same defect
        // as HORO-1507's self-creating settings file, and worse: it would mean
        // every read of the baseline added to it, so the observation count
        // would measure how often somebody looked (HORO-1547).
        (
            "workflow-profile show".to_string(),
            argv(&["workflow-profile", "show"]),
        ),
        (
            "workflow-profile show".to_string(),
            argv(&["workflow-profile", "show", "--json"]),
        ),
        // The bare command, because `show` is its default: if the default verb
        // were ever changed to `record`, typing the command name would start
        // writing and nothing else here would notice.
        (
            "workflow-profile show".to_string(),
            argv(&["workflow-profile"]),
        ),
    ]
}

/// The invocations covering surfaces declared with `safety`, having first
/// checked that the invocation list and the table agree about who is in which
/// set.
///
/// The coverage check is over *both* labels at once, deliberately. Checking
/// one at a time would let a surface move from `ReadOnly` to
/// `ConsultsInstalledTools` — the weaker claim — and disappear from the strict
/// test without anything noticing that it had stopped being covered.
fn invocations_declared(scratch: &Path, safety: Safety) -> Vec<(String, Vec<String>)> {
    let invocations = inspect_invocations(scratch);

    let covered: BTreeSet<String> = invocations.iter().map(|(s, _)| s.clone()).collect();
    let required: BTreeSet<String> = INSPECTING_LABELS
        .iter()
        .flat_map(|label| surfaces_declared(*label))
        .collect();
    assert_eq!(
        covered, required,
        "the surfaces this test runs are not the ones the table declares as looking \
         but not writing; a surface with no invocation here would be asserted about \
         by nobody"
    );

    invocations
        .into_iter()
        .filter(|(surface, _)| declared_safety(surface) == safety)
        .collect()
}

/// AC 2 and AC 3, observed: every surface labelled "Read-only — changes
/// nothing." leaves the tree it would write to byte-for-byte identical, and
/// `daemon status` is among them rather than having been relabelled away.
///
/// Runs with no tool reachable on `PATH`. This is the deterministic form of
/// the claim — it holds on a machine with no build tools installed as much as
/// on this one — and the companion test below runs the same set against the
/// real `PATH`, which is the configuration a user is in.
#[test]
fn read_only_surfaces_leave_a_disposable_home_untouched() {
    let scratch = make_temp_dir("horo1485-scratch");
    fs::write(scratch.join("a-file"), vec![0u8; 1024]).expect("write scratch file");
    let no_tools = make_temp_dir("horo1485-no-tools");

    for (surface, args) in &invocations_declared(&scratch, Safety::ReadOnly) {
        let home = make_temp_dir("horo1485-home");
        let before = snapshot(&home);

        let output = run_with_home_and_no_tools(args, &home, &no_tools);
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

/// The strict claim in the configuration a user is actually in (HORO-1560):
/// with the machine's real `PATH`, a surface declared `ReadOnly` still leaves
/// its `HOME` byte-for-byte identical.
///
/// This assertion did not exist before the tool-consulting label did, and it
/// is the reason the label was added rather than the wording softened. While
/// `detect` was in this set the strict comparison could only be made with an
/// empty `PATH`, because `detect` runs build tools and they write on their own
/// account. Taking `detect` out leaves a set that starts no other program at
/// all — so there is nothing left that could write, and the strongest form of
/// the claim becomes provable exactly where it matters.
///
/// `only_the_tool_consulting_surfaces_start_another_program` is what keeps
/// that reasoning honest: it observes that these surfaces start nothing,
/// rather than assuming it from the label.
#[test]
fn read_only_surfaces_leave_a_disposable_home_untouched_with_the_real_path() {
    let scratch = make_temp_dir("horo1560-scratch");
    fs::write(scratch.join("a-file"), vec![0u8; 1024]).expect("write scratch file");

    for (surface, args) in &invocations_declared(&scratch, Safety::ReadOnly) {
        let home = make_temp_dir("horo1560-home");
        let before = snapshot(&home);

        let output = run_with_home(args, &home);
        assert!(
            output.status.code().is_some(),
            "`glomeris {}` was killed by a signal rather than exiting",
            args.join(" ")
        );

        let after = snapshot(&home);
        assert_eq!(
            before,
            after,
            "'{surface}' is declared \"Read-only — changes nothing.\" but \
             `glomeris {}` changed its HOME on a machine with the real PATH \
             (exit {:?}). If this surface has started consulting an installed \
             tool, the honest fix is Safety::ConsultsInstalledTools, not a \
             looser comparison. Paths after: {:?}",
            args.join(" "),
            output.status.code(),
            after.keys().collect::<Vec<_>>()
        );

        fs::remove_dir_all(&home).ok();
    }

    fs::remove_dir_all(&scratch).ok();
}

/// The claim a tool-consulting surface *does* make (HORO-1556, HORO-1560):
/// with the machine's real `PATH`, and therefore the real `npm`, `go`, `pip`,
/// `uv`, `brew` and `docker` running, it creates no state Glomeris owns.
///
/// What a foreign tool wrote is recorded in the assertion message rather than
/// asserted to be nothing. Glomeris cannot enforce a zero there — `go env`
/// writes a telemetry counter no environment can suppress — and a test that
/// asserted one anyway would pass or fail according to which build tools the
/// machine happens to have. What is enforceable, and enforced, is that none of
/// those writes are Glomeris's: a surface that reported your settings by
/// writing them out, or answered "is a notification owed?" by opening an
/// episode, would put a file under `Library/Application Support/Glomeris` and
/// fail here.
#[test]
fn a_tool_consulting_surface_writes_no_glomeris_state_and_its_foreign_writes_are_recorded() {
    let scratch = make_temp_dir("horo1556-scratch");
    fs::write(scratch.join("a-file"), vec![0u8; 1024]).expect("write scratch file");

    let consulting = invocations_declared(&scratch, Safety::ConsultsInstalledTools);
    // The set is derived from the table, so an empty one would agree with
    // every assertion in the loop and report a pass. `detect` is in it.
    assert!(
        !consulting.is_empty(),
        "no surface is declared Safety::ConsultsInstalledTools, so this test \
         asserts nothing — and `detect` runs build tools whatever the table says"
    );

    for (surface, args) in &consulting {
        let home = make_temp_dir("horo1556-home");

        let output = run_with_home(args, &home);
        assert!(
            output.status.code().is_some(),
            "`glomeris {}` was killed by a signal rather than exiting",
            args.join(" ")
        );

        let (owned, foreign): (Vec<PathBuf>, Vec<PathBuf>) =
            snapshot(&home).into_keys().partition(|path| {
                GLOMERIS_OWNED_PREFIXES
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            });
        assert!(
            owned.is_empty(),
            "'{surface}' is declared to write nothing of its own but `glomeris {}` \
             created state Glomeris owns (exit {:?}): {owned:?}. Foreign writes \
             observed alongside it: {foreign:?}",
            args.join(" "),
            output.status.code()
        );
        // A label that promised the filesystem was untouched would be a false
        // claim the moment a foreign tool wrote anything here — which is the
        // defect HORO-1560 was filed for.
        assert!(
            foreign.is_empty() || !declared_safety(surface).promises_nothing_changes(),
            "'{surface}' promises nothing changes, and yet running `glomeris {}` \
             left {} path(s) behind that Glomeris does not own: {foreign:?}",
            args.join(" "),
            foreign.len()
        );

        fs::remove_dir_all(&home).ok();
    }

    fs::remove_dir_all(&scratch).ok();
}

/// A directory of executables named after [`SPAWNED_PROGRAMS`], each of which
/// records that it was run and answers nothing.
///
/// Answering nothing is deliberate: a stub that printed a plausible cache path
/// would send the detector off to measure a directory, which is a second thing
/// for the test to reason about. An empty answer reaches the detector as a
/// failed probe, and a failed probe is not what is under test here — *that the
/// program started at all* is.
fn recording_stub_path(dir: &Path, log: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("create stub bin dir");
    for program in SPAWNED_PROGRAMS {
        let stub = bin.join(program);
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' {program} >> '{}'\nexit 0\n",
                log.display()
            ),
        )
        .expect("write stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");
    }
    bin
}

/// AC 5, and what makes the split more than a rename: which surfaces start
/// another program is *observed*, and each label is checked against the
/// observation (HORO-1560).
///
/// Every inspecting surface is run against a `PATH` containing nothing but
/// recording stubs named after the programs a detector can spawn. A surface
/// declared "Read-only — changes nothing." must start none of them, because
/// that label is a promise about the whole filesystem and Glomeris cannot make
/// it on another program's behalf. A surface declared to consult installed
/// tools must start at least one, so the warning is not being handed out to
/// verbs that do not need it — a reader who is warned once for nothing
/// discounts the next warning too.
///
/// Both directions bite. Declaring `detect` `ReadOnly` again fails the first
/// assertion; labelling a surface that runs nothing as tool-consulting fails
/// the second.
#[test]
fn only_the_tool_consulting_surfaces_start_another_program() {
    let scratch = make_temp_dir("horo1560-spawn-scratch");
    fs::write(scratch.join("a-file"), vec![0u8; 1024]).expect("write scratch file");
    let stubs = make_temp_dir("horo1560-stubs");
    let log = stubs.join("invoked.log");
    let stub_path = recording_stub_path(&stubs, &log);

    let mut anything_spawned = false;
    for (surface, args) in &inspect_invocations(&scratch) {
        let home = make_temp_dir("horo1560-spawn-home");
        fs::write(&log, "").expect("truncate the invocation log");

        let output = Command::new(glomeris_bin())
            .args(args)
            .env("HOME", &home)
            .env("PATH", &stub_path)
            .stdin(Stdio::null())
            .output()
            .expect("failed to spawn glomeris binary");
        assert!(
            output.status.code().is_some(),
            "`glomeris {}` was killed by a signal rather than exiting",
            args.join(" ")
        );

        let mut spawned: Vec<String> = fs::read_to_string(&log)
            .expect("read the invocation log")
            .lines()
            .map(str::to_string)
            .collect();
        spawned.sort();
        spawned.dedup();
        anything_spawned |= !spawned.is_empty();

        match declared_safety(surface) {
            Safety::ReadOnly => assert!(
                spawned.is_empty(),
                "'{surface}' is declared \"Read-only — changes nothing.\" but \
                 `glomeris {}` started {spawned:?}. Glomeris cannot promise that \
                 another program changes nothing — a version manager fronting one \
                 of these can install a whole toolchain. Declare this surface \
                 Safety::ConsultsInstalledTools instead",
                args.join(" ")
            ),
            Safety::ConsultsInstalledTools => assert!(
                !spawned.is_empty(),
                "'{surface}' is declared to run installed tools, but `glomeris {}` \
                 started none of {SPAWNED_PROGRAMS:?}. A warning given to a verb \
                 that does not need it teaches the reader to ignore the next one",
                args.join(" ")
            ),
            other => panic!("'{surface}' reached this test with label {other:?}"),
        }

        fs::remove_dir_all(&home).ok();
    }

    // The stubs are only reachable if the binary resolves these programs
    // through `PATH` at all. If it stopped doing so — a hardcoded
    // `/opt/homebrew/bin/brew`, say — every surface would look like it started
    // nothing and this test would read as a pass.
    assert!(
        anything_spawned,
        "no surface started any of {SPAWNED_PROGRAMS:?}, so the stub PATH observed \
         nothing and every assertion above was vacuous"
    );

    fs::remove_dir_all(&stubs).ok();
    fs::remove_dir_all(&scratch).ok();
}

/// The control that makes both tests above mean something.
///
/// If the binary resolved its state directory from anything other than `HOME`
/// — a hardcoded path, a cached value, `NSHomeDirectory()` — then every
/// assertion above would pass while the commands wrote to the real home
/// directory, and this suite would be certifying the opposite of what it
/// claims. So a surface that is *declared* to write is run the same way, and
/// the disposable `HOME` is required to change — under
/// [`GLOMERIS_OWNED_PREFIXES`], which is also what makes the real-`PATH`
/// test's filter non-vacuous.
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
    // The same prefix the real-`PATH` test filters on, so the two cannot
    // drift: this is what proves that filter can actually catch something.
    assert!(
        written.iter().all(|path| GLOMERIS_OWNED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
            || path == &&PathBuf::from("Library")
            || path == &&PathBuf::from("Library/Application Support")),
        "`autopilot enable` wrote outside Glomeris's own state directory: {written:?}"
    );
    assert!(
        written.iter().any(|path| path.ends_with("autopilot.conf")),
        "`autopilot enable` did not write the envelope: {written:?}"
    );
}
