//! CLI integration proof that every help surface renders from the one command
//! table in `glomeris::cli::help`, so they cannot independently drift the way
//! `print_usage()` once omitted `llm-plan` (HORO-1034).
//!
//! # Why this file no longer keeps its own subcommand list
//!
//! It used to. HORO-1050 moved `--help` and the unknown-command usage onto a
//! shared `COMMANDS` table, but that table lived in `src/main.rs` — invisible
//! to any test — so this file carried a hand-written `ALL_SUBCOMMANDS` mirror
//! of it. The mirror is exactly the thing the shared table was introduced to
//! abolish, and it drifted in precisely the same way: by HORO-1311 it was
//! missing `llm-check`, so "every subcommand supports its own `--help`" was
//! quietly being asserted over twelve of the thirteen subcommands, and the
//! thirteenth was the one most recently added.
//!
//! HORO-1311 moved the table into the library. This file now reads
//! `help::COMMANDS` directly. There is no list here to fall out of date, and
//! a fourteenth subcommand is covered by every test below the moment its row
//! exists.
//!
//! Still spawns the real binary — same `glomeris_bin()` idiom as
//! `tests/history_cli.rs` — because what is under test is what the process
//! actually prints, not what a renderer returns in isolation. The renderers
//! are unit-tested inside `src/cli/help.rs`.

use std::process::{Command, Stdio};

use glomeris::cli::help;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

fn stdout_of(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "`glomeris {}` must succeed; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn top_level_help_lists_every_subcommand() {
    let stdout = stdout_of(&["--help"]);

    for command in help::COMMANDS {
        assert!(
            stdout.contains(command.name),
            "--help output is missing subcommand '{}': {stdout}",
            command.name
        );
    }
}

/// The failure this file exists to catch, in the direction that actually bit:
/// a subcommand the binary dispatches that help never mentions.
///
/// Checked by reading `main.rs`'s dispatch arms rather than by running the
/// binary. The first version of this test did probe at runtime — it passed each
/// name a flag no parser accepts and looked for `unknown command` in the
/// reply — and that was a mistake, because `glomeris emergency` discarded its
/// arguments entirely, so probing it *performed a real emergency recovery run*.
/// It ran three times before the behaviour was noticed, and the only thing that
/// kept the damage to Glomeris's own 50-byte history file was that the one
/// other action it attempted happened to fail.
///
/// `emergency` now rejects unknown arguments, but a test whose safety rests on
/// argument parsing staying strict is a test that deletes data the day someone
/// relaxes it. Nothing here executes a subcommand.
#[test]
fn help_mentions_every_subcommand_the_binary_actually_dispatches() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("failed to read src/main.rs");

    let body = source
        .split_once("    match args.first().map(String::as_str) {")
        .expect("main's dispatch match no longer looks the way this test expects")
        .1
        .split_once("\n        Some(other) => {")
        .expect("main's dispatch match no longer ends with a catch-all arm")
        .0;

    let mut dispatched: Vec<&str> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Some(\"") else {
            continue;
        };
        let Some((name, _)) = rest.split_once('"') else {
            continue;
        };
        dispatched.push(name);
    }

    // The flags and the `help` topic entry point are dispatched here too and
    // are deliberately not subcommands.
    let not_a_subcommand = ["--help", "-h", "help", "--version", "-V"];
    let mut dispatched: Vec<&str> = dispatched
        .into_iter()
        .filter(|name| !not_a_subcommand.contains(name))
        .collect();
    dispatched.sort_unstable();
    dispatched.dedup();

    assert!(
        dispatched.len() > 5,
        "parsed only {dispatched:?} out of main's dispatch — this test's parsing has broken, \
         and a broken parse here reads as a pass"
    );

    let mut tabled: Vec<&str> = help::COMMANDS.iter().map(|c| c.name).collect();
    tabled.sort_unstable();

    assert_eq!(
        dispatched, tabled,
        "main.rs dispatches a different set of subcommands than help describes"
    );
}

#[test]
fn unknown_command_exits_two_and_points_at_help_instead_of_reprinting_it() {
    let output = run(&["badcmd"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("unknown command 'badcmd'"), "{stderr}");
    assert!(stderr.contains("glomeris --help"), "{stderr}");

    // The pre-HORO-1311 behaviour was to print the aggregate usage of all
    // thirteen subcommands here: a 13-line, 146-column wall in answer to one
    // mistyped word. A bad command earns a pointer, not the manual.
    assert!(
        stderr.lines().count() <= 6,
        "the unknown-command message is back to being a wall of text:\n{stderr}"
    );
    for line in stderr.lines() {
        assert!(
            line.chars().count() <= 80,
            "unknown-command message has a {}-column line: {line}",
            line.chars().count()
        );
    }
}

#[test]
fn every_subcommand_supports_its_own_help_flag() {
    for command in help::COMMANDS {
        for flag in ["--help", "-h"] {
            let stdout = stdout_of(&[command.name, flag]);
            // Asserted positively — that the output opens with this command's
            // own help header — rather than by looking for an error phrase in
            // it. A negative check here would be unsound: `llm-plan --help`
            // legitimately explains that exit 2 covers "an unrecognized
            // argument", so scanning for that phrase flags correct output.
            assert!(
                stdout.starts_with(&format!("glomeris {} — ", command.name)),
                "'glomeris {} {flag}' did not print this command's help: {stdout}",
                command.name
            );
        }
    }
}

/// AC 2: coherent per-command help means it actually carries the parts, not
/// just that it exits zero. Options are conditional — `scan` and `emergency`
/// legitimately take none — but a usage line, a safety statement and at least
/// one example are not optional for anything.
#[test]
fn every_subcommand_help_carries_usage_safety_and_an_example() {
    for command in help::COMMANDS {
        let stdout = stdout_of(&[command.name, "--help"]);

        assert!(
            stdout.contains(&format!("usage: glomeris {}", command.usage[0])),
            "'glomeris {} --help' has no usage line: {stdout}",
            command.name
        );
        assert!(
            stdout.contains(command.safety.label()),
            "'glomeris {} --help' does not state whether it changes anything: {stdout}",
            command.name
        );
        assert!(
            stdout.contains("Examples\n"),
            "'glomeris {} --help' has no examples: {stdout}",
            command.name
        );
        // Both directions: a command with options must show them, and a
        // command with none must not print an empty heading.
        assert_eq!(
            stdout.contains("Options\n"),
            !command.options.is_empty(),
            "'glomeris {} --help' renders an Options block that does not match its spec",
            command.name
        );
    }
}

/// AC 7: the exit-code reference is reachable without being in the way.
#[test]
fn exit_code_guidance_is_a_topic_rather_than_part_of_primary_help() {
    let topic = stdout_of(&["help", "exit-codes"]);
    assert!(topic.contains("75"), "{topic}");
    assert!(topic.contains("execution lock"), "{topic}");

    // Top-level help points at it and does not inline it.
    let top = stdout_of(&["--help"]);
    assert!(top.contains("glomeris help exit-codes"), "{top}");
    assert!(
        !top.contains("execution lock"),
        "top-level help has absorbed the exit-code reference:\n{top}"
    );
}

#[test]
fn an_unknown_help_topic_lists_the_topics_that_exist() {
    let output = run(&["help", "not-a-topic"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    for (topic, _) in help::HELP_TOPICS {
        assert!(
            stderr.contains(topic),
            "topic '{topic}' not listed: {stderr}"
        );
    }
}

/// `help <command>` is what someone means when they type it, and answering
/// the question is better than being correct about the syntax.
#[test]
fn help_accepts_a_command_name_as_well_as_a_topic() {
    for command in help::COMMANDS {
        assert_eq!(
            stdout_of(&["help", command.name]),
            stdout_of(&[command.name, "--help"]),
            "`glomeris help {}` and `glomeris {} --help` disagree",
            command.name,
            command.name
        );
    }
}

/// `help` with no topic is the same request as `--help`.
#[test]
fn bare_help_and_the_help_flag_render_identically() {
    let flag = stdout_of(&["--help"]);
    assert_eq!(stdout_of(&["help"]), flag);
    assert_eq!(stdout_of(&["-h"]), flag);
}

/// A usage error inside a subcommand must answer with *that subcommand's*
/// usage. Before HORO-1311 every one of them printed the aggregate for all
/// thirteen, so getting one `free` flag wrong told you about `daemon`.
#[test]
fn a_subcommand_usage_error_prints_that_subcommands_usage_only() {
    let output = run(&["free", "--not-a-real-flag"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("usage: glomeris free"), "{stderr}");
    assert!(stderr.contains("glomeris free --help"), "{stderr}");

    for other in help::COMMANDS.iter().filter(|c| c.name != "free") {
        assert!(
            !stderr.contains(&format!("usage: glomeris {}", other.name)),
            "`free`'s usage error also describes '{}':\n{stderr}",
            other.name
        );
    }
}

/// `--version` exists and agrees with the version help reports about itself.
/// Added in HORO-1311 because a bare invocation printing a bare version
/// string was the only way to get it, which is not where anyone looks.
#[test]
fn version_is_reported_by_flag_and_by_bare_invocation() {
    let expected = format!("glomeris {}", env!("CARGO_PKG_VERSION"));

    for flag in ["--version", "-V"] {
        assert_eq!(stdout_of(&[flag]).trim_end(), expected);
    }

    let bare = stdout_of(&[]);
    assert!(bare.starts_with(&expected), "{bare}");
    assert!(
        bare.contains("glomeris --help"),
        "a bare invocation should say where to go next: {bare}"
    );
    assert!(stdout_of(&["--help"]).contains(&expected), "{expected}");
}
