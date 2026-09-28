use glomeris::cli::help;
use glomeris::{monitor, platform};
use std::path::PathBuf;

/// The command table that both `--help` and the unknown-command usage render
/// from lives in [`glomeris::cli::help`], not here (HORO-1311).
///
/// It started here in HORO-1050, which fixed the original drift (HORO-1034:
/// `print_usage()` had silently omitted `llm-plan`) by making both paths read
/// one array. Keeping that array in the binary crate meant nothing but a
/// spawned-process test could see it, and the test that checked it kept a
/// hand-written mirror of the subcommand list — a mirror which had itself
/// drifted by the time HORO-1311 started, missing `llm-check`. Moving the
/// table into the library removes the need for any mirror: tests read
/// `help::COMMANDS` directly.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `glomeris <sub> --help`/`-h` (HORO-1050): looked up in the same
    // COMMANDS table before falling through to the subcommand's own
    // dispatch/parsing, so every subcommand supports its own `--help`
    // uniformly rather than as a one-off special case.
    if let Some(name) = args.first() {
        if let Some(spec) = help::find_command(name) {
            if matches!(args.get(1).map(String::as_str), Some("--help") | Some("-h")) {
                print_help(&help::render_command_help(spec));
                return;
            }
        }
    }

    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") => {
            print_help(&help::render_top_level_help(env!("CARGO_PKG_VERSION")));
        }
        // `help` with no topic is the same request as `--help`; `help <topic>`
        // is AC 7's progressive disclosure, where the reference material a
        // reader wants on their second day lives without crowding the help
        // they need on their first.
        Some("help") => run_help_command(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("glomeris {}", env!("CARGO_PKG_VERSION"));
        }
        Some("daemon") => run_daemon_command(&args[1..]),
        Some("actions") => run_actions_command(&args[1..]),
        Some("scan") => {
            if let Err(e) = glomeris::scanner::run_scan_cli(&args[1..]) {
                eprintln!("glomeris scan: {e}");
                print_command_usage("scan");
                std::process::exit(2);
            }
        }
        Some("status") => run_status_command(&args[1..]),
        Some("detect") => run_detect_command(&args[1..]),
        Some("explain") => run_explain_command(&args[1..]),
        Some("clean") => run_clean_command(&args[1..]),
        Some("llm-plan") => run_llm_plan_command(&args[1..]),
        Some("llm-check") => run_llm_check_command(&args[1..]),
        Some("execute") => run_execute_command(&args[1..]),
        Some("emergency") => run_emergency_command(&args[1..]),
        Some("history") => run_history_command(&args[1..]),
        Some("free") => run_free_command(&args[1..]),
        Some("autopilot") => run_autopilot_command(&args[1..]),
        Some("settings") => run_settings_command(&args[1..]),
        Some("pressure") => run_pressure_command(&args[1..]),
        Some(other) => {
            eprintln!("glomeris: unknown command '{other}'");
            eprintln!("{}", help::render_unknown_command_hint());
            std::process::exit(2);
        }
        None => {
            // A bare invocation used to print only the version, which told a
            // first-time reader the binary exists and nothing about how to use
            // it. The version line stays — scripts and bug reports rely on it —
            // with one line added pointing at the help that now has something
            // worth reading.
            println!("glomeris {}", env!("CARGO_PKG_VERSION"));
            println!("Run `glomeris --help` to see what it can do.");
        }
    }
}

/// `glomeris help [topic]`.
///
/// No topic is the same request as `--help`. An unknown topic lists the topics
/// that exist rather than guessing, and exits 2 like every other usage error.
fn run_help_command(args: &[String]) {
    match args.first().map(String::as_str) {
        None => print_help(&help::render_top_level_help(env!("CARGO_PKG_VERSION"))),
        Some("exit-codes") => print_help(&help::render_exit_codes()),
        // A command name here is what someone means by `glomeris help detect`,
        // and refusing it on a technicality when the answer is one function
        // call away would be pedantry rather than a safety property.
        Some(name) if help::find_command(name).is_some() => {
            let spec = help::find_command(name).expect("checked by the guard above");
            print_help(&help::render_command_help(spec));
        }
        Some(other) => {
            eprintln!("glomeris: no help topic '{other}'");
            eprintln!("{}", help::render_topic_list());
            std::process::exit(2);
        }
    }
}

/// Writes help text to stdout, treating a closed pipe as a normal ending.
///
/// `println!` panics on `EPIPE`, which used to be nearly unobservable: help was
/// thirteen lines, so nobody piped it anywhere. Now that it is a screenful,
/// `glomeris --help | head` is an ordinary thing to type, and a panic message
/// is the wrong answer to it — the reader got the lines they asked for and
/// stopped reading, which is not an error. Only the help paths need this; a
/// report cut short by a closed pipe is genuinely incomplete output and keeps
/// the default behaviour.
fn print_help(text: &str) {
    use std::io::Write;

    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            eprintln!("glomeris: failed writing help to stdout: {e}");
            std::process::exit(1);
        }
    }
}

/// Prints one subcommand's own usage line on a usage error in that
/// subcommand, plus where to read more.
///
/// Before HORO-1311 every usage error in every subcommand printed the
/// aggregate usage for all thirteen — so mistyping one `free` flag produced a
/// 13-line, 146-column wall in which the reader had to locate their own
/// command before they could find their mistake. The answer to "you got
/// `free`'s flags wrong" is `free`'s flags.
fn print_command_usage(name: &str) {
    match help::find_command(name) {
        Some(spec) => {
            eprint!("{}", help::render_command_usage(spec));
            eprintln!("Run `glomeris {name} --help` for options and examples.");
        }
        // Unreachable while every caller passes a literal that is in the
        // table, and asserted as such by test. Falling back to the general
        // usage line rather than panicking, because a help path is the worst
        // possible place to abort a process.
        None => eprintln!("{}", help::render_unknown_command_hint()),
    }
}

/// The refusal a subcommand prints for an argument it cannot account for,
/// followed by that subcommand's own usage, then exit 2.
///
/// `command` is the command as a user typed it, so a nested verb reads back
/// as `glomeris actions list: ...`; the usage underneath is the table entry
/// for its first word, because [`help::COMMANDS`] is keyed by subcommand.
/// That is exactly what `actions history` already did by hand — shared here
/// rather than restated a fifth time (HORO-1322).
fn usage_error(command: &str, arg: &str) -> ! {
    eprintln!("glomeris {command}: unrecognized argument '{arg}'");
    print_command_usage(command.split_whitespace().next().unwrap_or(command));
    std::process::exit(2);
}

/// Parses a flat argument list into a positional-args list and a set of
/// bare `--flag` switches (no `--flag value` pairs handled here — callers
/// that need a valued flag, e.g. `--target`, parse that one explicitly
/// before calling this on what remains). Never panics on malformed input.
///
/// A token starting with `-` must be in `known_flags`, and is returned as
/// `Err` otherwise. It used to become a *positional* instead (HORO-1322),
/// which meant `detect --jsonn` ran a full discovery and printed prose while
/// exiting 0, and `explain --resource-id <id>` searched for a candidate
/// literally named `--resource-id` and reported it as not found — a syntax
/// mistake reported as a fact about the machine.
///
/// A dash token is never a positional here, deliberately: none of the
/// callers has a valued flag left to parse by this point, so there is
/// nothing such a token could mean except a flag that does not exist. A
/// genuine path beginning with a dash is still reachable as `./-name`.
fn split_flags<'a>(
    args: &'a [String],
    known_flags: &[&str],
) -> Result<(Vec<&'a str>, Vec<&'a str>), &'a str> {
    let mut positionals = Vec::new();
    let mut flags = Vec::new();
    for arg in args {
        let arg = arg.as_str();
        if known_flags.contains(&arg) {
            flags.push(arg);
        } else if arg.starts_with('-') {
            return Err(arg);
        } else {
            positionals.push(arg);
        }
    }
    Ok((positionals, flags))
}

/// [`split_flags`] for a subcommand that takes no positional arguments at
/// all — `status`, `detect`, `actions list`, `daemon status`. Refuses an
/// unknown flag *and* a stray positional through [`usage_error`], so all
/// four read identically to `clean` and `emergency`, which were already
/// strict (HORO-1322).
fn flags_only<'a>(command: &str, args: &'a [String], known_flags: &[&str]) -> Vec<&'a str> {
    match split_flags(args, known_flags) {
        Err(unknown) => usage_error(command, unknown),
        Ok((positionals, flags)) => match positionals.first() {
            Some(stray) => usage_error(command, stray),
            None => flags,
        },
    }
}

fn print_json_or_exit(value: &impl serde::Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("glomeris: failed to render JSON report: {e}");
            std::process::exit(1);
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Builds the standard evidence-correlation-classification pipeline every
/// `detect`/`explain`/`clean` subcommand uses: real detectors, the real
/// `DefaultEvidenceCollector`, and the default policy config, evaluated at
/// the current wall-clock time. `project_roots` (HORO-957) is threaded into
/// the `DiscoveryContext` so the cargo/node detectors — which only ever scan
/// paths under `known_project_roots` — can actually find anything; it is
/// empty when the caller passed no `--project-root` flags, preserving the
/// prior behavior for anyone who doesn't use the new flag.
fn discover_and_classify_now(
    project_roots: Vec<PathBuf>,
) -> Vec<(
    glomeris::evidence::Evidence,
    glomeris::policy::PolicyDecision,
)> {
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::policy::PolicyConfig;
    use std::time::SystemTime;

    let ctx = DiscoveryContext::new(home_dir()).with_known_project_roots(project_roots);
    let registry = DetectorRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();
    let cfg = PolicyConfig::default();
    let now = SystemTime::now();

    glomeris::cli::discover_and_classify(&registry, &ctx, &collector, &cfg, now)
}

/// Same discovery pipeline as [`discover_and_classify_now`], but behind
/// `--progress-json` (HORO-1052): emits one NDJSON-encoded
/// [`glomeris::reporting::dto::ProgressEvent`] line to stderr per
/// detector start/finish as `detect`/`explain`/`llm-plan`'s shared
/// discovery phase runs. When `progress_json` is `false` this emits
/// nothing at all — stdout and stderr stay byte-identical to
/// [`discover_and_classify_now`]'s behavior, which is the ticket's
/// explicit, testable AC.
fn discover_and_classify_now_with_progress(
    project_roots: Vec<PathBuf>,
    progress_json: bool,
) -> Vec<(
    glomeris::evidence::Evidence,
    glomeris::policy::PolicyDecision,
)> {
    discover_pass_now(project_roots, progress_json).candidates
}

/// Same single discovery pass as [`discover_and_classify_now_with_progress`]
/// — the same detectors, run the same once — but keeps what each detector
/// reported alongside the candidates it produced (HORO-1487), for the one
/// caller that needs both: `detect`'s human output, which prints a line per
/// detector above the report.
fn discover_pass_now(
    project_roots: Vec<PathBuf>,
    progress_json: bool,
) -> glomeris::cli::DiscoveryPass {
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::policy::PolicyConfig;
    use std::time::SystemTime;

    let ctx = DiscoveryContext::new(home_dir()).with_known_project_roots(project_roots);
    let registry = DetectorRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();
    let cfg = PolicyConfig::default();
    let now = SystemTime::now();

    glomeris::cli::discover_and_classify_pass(&registry, &ctx, &collector, &cfg, now, |event| {
        if progress_json {
            match serde_json::to_string(&event) {
                Ok(line) => eprintln!("{line}"),
                Err(e) => {
                    eprintln!("glomeris: failed to render progress event: {e}");
                }
            }
        }
    })
}

/// `glomeris status` — current disk pressure state (HORO-955).
#[cfg(target_os = "macos")]
fn run_status_command(args: &[String]) {
    use glomeris::monitor::{FsStat, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;

    let flags = flags_only("status", args, &["--json"]);
    let fs_stat = MacosFsStat;
    let usage = match fs_stat.stat(std::path::Path::new("/")) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("glomeris status: failed to read filesystem usage: {e}");
            std::process::exit(1);
        }
    };

    let report = glomeris::cli::build_status_report(&usage, &ThresholdConfig::default());
    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_status_report(&report);
    }
}

#[cfg(not(target_os = "macos"))]
fn run_status_command(_args: &[String]) {
    eprintln!("glomeris status: only supported on macOS");
    std::process::exit(1);
}

/// Free space on the root filesystem, for HORO-1307's relative
/// storage-impact thresholds ("this cache is a quarter of everything you
/// have left").
///
/// Returns [`ImpactContext::default`] — i.e. no disk context at all — when
/// the reading is unavailable, which is the correct behaviour rather than a
/// swallowed error: `detect` is cross-platform and the impact model is
/// explicitly designed to fall back to its absolute thresholds. A failed
/// `statfs` must not make `detect` exit non-zero, because the candidate list
/// is still completely valid without it. Guessing a capacity instead would
/// produce confidently wrong tiers.
#[cfg(target_os = "macos")]
fn impact_context() -> glomeris::reporting::ImpactContext {
    use glomeris::monitor::FsStat;
    use glomeris::platform::macos::MacosFsStat;

    match MacosFsStat.stat(std::path::Path::new("/")) {
        Ok(usage) => glomeris::reporting::ImpactContext::with_free_bytes(usage.free_bytes),
        Err(_) => glomeris::reporting::ImpactContext::default(),
    }
}

/// Non-macOS builds have no `FsStat` implementation, so the impact model
/// runs on its absolute thresholds alone. See the macOS variant above.
#[cfg(not(target_os = "macos"))]
fn impact_context() -> glomeris::reporting::ImpactContext {
    glomeris::reporting::ImpactContext::default()
}

/// Exit code used when [`glomeris::executor::lock::acquire_execution_lock`]
/// reports [`glomeris::executor::lock::LockError::AlreadyHeld`] — a
/// distinct "busy" signal (loosely following `sysexits.h`'s `EX_TEMPFAIL`)
/// rather than a generic failure, so a caller/script can tell "another
/// real execution is already in progress, retry later" apart from "this
/// invocation itself failed".
const EXIT_EXECUTION_LOCK_BUSY: i32 = 75;

/// Acquires the standalone HORO-1054 execution lock or exits with
/// [`EXIT_EXECUTION_LOCK_BUSY`]/a generic failure, printing `command_name`
/// in the error message. Shared by `run_emergency_command`, `free_run`,
/// and `run_execute_command` — the three real-execution entry points.
///
/// `json`: HORO-1056 gap fix. `emergency` has no `--json` mode at all, so it
/// always passes `false` here and behaves exactly as before. `execute`
/// (HORO-1055) and `free` (HORO-1506) DO, and both parse it before ever
/// reaching this call site — passing it through here means a `--json` caller
/// gets a structured `"busy"` report on stdout instead of silence plus a bare
/// exit code, matching every other refusal path
/// `render_execute_resolution` already renders. This is the one
/// refusal/abort path in `execute` that happens BEFORE
/// `glomeris::cli::ExecuteResolution` exists at all, which is why it is
/// not one of that enum's variants and is rendered here instead.
///
/// The shape a `--json` caller gets here is `ExecuteRefusalReport`, not that
/// command's own report — a busy lock means the run never started, so there
/// is no run to report on. `execute --json` has always behaved this way and
/// `free --json` follows it rather than inventing a second convention.
#[cfg(target_os = "macos")]
fn acquire_execution_lock_or_exit(
    command_name: &str,
    json: bool,
) -> glomeris::executor::lock::ExecutionLockGuard {
    use glomeris::executor::lock::{acquire_execution_lock, LockError};
    use glomeris::reporting::dto::{ExecuteRefusalReport, RefusalReason};

    match acquire_execution_lock() {
        Ok(guard) => guard,
        Err(LockError::AlreadyHeld) => {
            if json {
                print_json_or_exit(&ExecuteRefusalReport {
                    reason: RefusalReason::Busy,
                    message: "another glomeris execution is already in progress (execution \
                              lock busy) — try again shortly"
                        .to_string(),
                });
            } else {
                eprintln!(
                    "glomeris {command_name}: another glomeris execution is already in progress \
                     (execution lock busy) — try again shortly"
                );
            }
            std::process::exit(EXIT_EXECUTION_LOCK_BUSY);
        }
        Err(LockError::Io(e)) => {
            eprintln!("glomeris {command_name}: failed to acquire the execution lock: {e}");
            std::process::exit(1);
        }
    }
}

/// `glomeris emergency` — the degraded-path recovery command (HORO-953).
/// Never touches network or an LLM provider; see
/// `glomeris::emergency`'s module docs for the full contract.
#[cfg(target_os = "macos")]
fn run_emergency_command(args: &[String]) {
    // Found while writing this command's help (HORO-1311): `emergency` took no
    // `args` slice at all, so every argument was discarded before dispatch and
    // `glomeris emergency --dry-run` performed a real, unannounced recovery
    // run. That is the worst place in the product to silently accept a flag,
    // because the flag a user is most likely to reach for here is the one that
    // means "don't actually do it". Every other subcommand rejects an
    // unrecognized argument with exit 2; this one now does too.
    //
    // `--help`/`-h` never arrive here — they are intercepted before dispatch.
    if let Some(unexpected) = args.first() {
        eprintln!("glomeris emergency: unrecognized argument '{unexpected}' — it takes none");
        print_command_usage("emergency");
        std::process::exit(2);
    }

    use glomeris::actions::ActionRegistry;
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::emergency::run_emergency;
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use std::time::Duration;

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns.
    let _execution_lock = acquire_execution_lock_or_exit("emergency", false);

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    // Same history path `daemon_run` uses — the pressure history the
    // polling loop appends to is what emergency mode treats as this tool's
    // self-owned disposable state (see the `emergency` module docs' step
    // 1). Since HORO-1467 emergency mode only ever deletes this file; it
    // no longer writes a record of its own into it. Whether deleting it is
    // right at all is HORO-1468.
    let self_state_path = home_dir.join("Library/Application Support/Glomeris/history.tsv");

    let ctx = DiscoveryContext::new(home_dir);
    let registry = DetectorRegistry::builtin();
    let actions = ActionRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();

    let report = run_emergency(
        &collector,
        &registry,
        &actions,
        &ctx,
        &self_state_path,
        20,
        Duration::from_secs(30),
        &actions_jsonl_path(),
    );

    print!("{report}");
}

#[cfg(not(target_os = "macos"))]
fn run_emergency_command(_args: &[String]) {
    eprintln!("glomeris emergency: only supported on macOS");
    std::process::exit(1);
}

/// `glomeris detect` — per-detector discovery status, plus (HORO-955) a
/// per-candidate report line showing reclaimable bytes and policy
/// classification.
fn run_detect_command(args: &[String]) {
    use glomeris::cli::DetectorOutcome;

    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris detect: {e}");
            print_command_usage("detect");
            std::process::exit(2);
        }
    };
    let flags = flags_only("detect", &remaining, &["--json", "--progress-json"]);
    let progress_json = flags.contains(&"--progress-json");

    // One discovery pass, both halves of the output (HORO-1487). The
    // per-detector lines and the report below them describe the same probe
    // of the same filesystem, because they come out of the same pass —
    // they used to come from two independent ones, which both doubled every
    // detector's cost and let the two halves disagree.
    let pass = discover_pass_now(project_roots, progress_json);

    if !flags.contains(&"--json") {
        for (id, outcome) in &pass.detectors {
            match outcome {
                DetectorOutcome::Found { candidates } => {
                    println!("{:<24} found ({} evidence)", id.0, candidates);
                }
                DetectorOutcome::ToolAbsent => {
                    println!("{:<24} tool_absent", id.0);
                }
                DetectorOutcome::Failed(reason) => {
                    println!("{:<24} failed: {reason}", id.0);
                }
            }
        }
    }

    let actions = glomeris::actions::ActionRegistry::builtin();
    // Both halves of the one pass (HORO-1484): the candidates, and the
    // health of every detector that produced them. `--json` used to carry
    // the first only, so a consumer could not tell a detector that found
    // nothing from one whose probe errored.
    let report = glomeris::cli::build_detect_report(
        &pass.candidates,
        &pass.detectors,
        &actions,
        impact_context(),
        // HORO-1511: the worktree-family grouping, so `detect` can answer
        // "which project is this 40 GiB" and not only "which directories".
        // Explanatory metadata — the candidate lines remain the only thing
        // that says what may run.
        &glomeris::cli::group_workspaces_now(&pass.candidates),
    );

    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_detect_report(&report);
    }
}

/// `glomeris explain <resource_id_or_path>` — full evidence-and-policy
/// picture for exactly one resource (HORO-955).
fn run_explain_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris explain: {e}");
            print_command_usage("explain");
            std::process::exit(2);
        }
    };
    let (positionals, flags) = match split_flags(&remaining, &["--json", "--progress-json"]) {
        Ok(v) => v,
        Err(unknown) => {
            eprintln!("glomeris explain: unrecognized argument '{unknown}'");
            // The shape HORO-1322 reported: someone reaches for
            // `--resource-id <id>` because that is how the id is named in
            // JSON output, and the only thing this command takes a flag for
            // is output format. Saying so is the difference between a
            // corrected invocation and a hunt for a resource that exists.
            eprintln!(
                "glomeris explain: the resource id or path is a positional argument, not a flag"
            );
            print_command_usage("explain");
            std::process::exit(2);
        }
    };
    let progress_json = flags.contains(&"--progress-json");

    // A second positional was silently dropped, so `explain a b` explained
    // `a` and said nothing about having been given two resources to explain.
    if let Some(extra) = positionals.get(1) {
        eprintln!("glomeris explain: unrecognized argument '{extra}'");
        eprintln!("glomeris explain: exactly one resource id or path is explained per invocation");
        print_command_usage("explain");
        std::process::exit(2);
    }

    let Some(query) = positionals.first() else {
        eprintln!("glomeris explain: a resource id or path argument is required");
        print_command_usage("explain");
        std::process::exit(2);
    };

    let candidates = discover_and_classify_now_with_progress(project_roots, progress_json);
    let Some((ev, decision)) = glomeris::cli::find_candidate(query, &candidates) else {
        eprintln!("glomeris explain: no discoverable candidate matches '{query}'");
        std::process::exit(1);
    };

    let actions = glomeris::actions::ActionRegistry::builtin();
    let report = glomeris::cli::build_explain_report(ev, decision, &actions);
    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_explain_report(&report);
    }
}

/// `glomeris clean --dry-run [--target <resource_id_or_path>]` — renders
/// what would be cleaned, without executing anything (HORO-955). There is
/// no non-dry-run execution path on this subcommand — see the PR's "Known
/// limitations": real destructive execution stays `free --target`'s job.
fn run_clean_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris clean: {e}");
            print_command_usage("clean");
            std::process::exit(2);
        }
    };

    let mut dry_run = false;
    let mut target: Option<&str> = None;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--target" => {
                target = remaining.get(i + 1).map(String::as_str);
                if target.is_none() {
                    eprintln!("glomeris clean: --target requires a value");
                    std::process::exit(2);
                }
                i += 2;
            }
            other => {
                eprintln!("glomeris clean: unrecognized argument '{other}'");
                print_command_usage("clean");
                std::process::exit(2);
            }
        }
    }

    if !dry_run {
        eprintln!(
            "glomeris clean: --dry-run is required; real destructive cleanup is not \
             implemented by this subcommand (use `glomeris free --target <N%|NB>` instead)"
        );
        std::process::exit(2);
    }

    use glomeris::actions::ActionRegistry;
    let candidates = discover_and_classify_now(project_roots);
    let actions = ActionRegistry::builtin();

    match glomeris::cli::build_clean_dry_run_report(&candidates, &actions, target) {
        Ok(report) => glomeris::cli::print_clean_dry_run_report(&report),
        Err(e) => {
            eprintln!("glomeris clean: {e}");
            std::process::exit(1);
        }
    }
}

/// `glomeris llm-plan [--project-root <path>]... [--plan-file <path>]
/// [--json]` — ADVISORY, NON-EXECUTING BYOK LLM suggestion surface
/// (HORO-1008). Never constructs a [`glomeris::policy::Approval`] and
/// never calls [`glomeris::policy::approval::authorize`] or
/// [`glomeris::executor::execute`] — see
/// [`glomeris::cli::build_llm_plan_report`]'s doc comment.
///
/// Without `--plan-file`, credentials are read only from
/// `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL` via
/// [`glomeris::actions::llm::provider_from_env`] — never accepted as a CLI
/// argument, to keep a key out of `ps`/shell history.
///
/// `glomeris llm-plan --schema` (HORO-1048) is a distinct, self-contained
/// mode: it prints [`glomeris::cli::llm_plan_schema_example`]'s example
/// `LlmPlan` JSON document to stdout and returns immediately, before any
/// discovery, `--plan-file` handling, or live-provider credential check
/// runs — it never touches project roots, evidence, or the LLM
/// configuration. The emitted document is that ticket's answer to the
/// v0.2.0 founder-dogfood finding that constructing a valid `--plan-file`
/// fixture required reading this module's `LlmPlanItem` struct directly.
fn run_llm_plan_command(args: &[String]) {
    use glomeris::actions::llm::{provider_from_env, FilePlanProvider};
    use glomeris::actions::ActionRegistry;

    let (project_roots, after_roots) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris llm-plan: {e}");
            print_command_usage("llm-plan");
            std::process::exit(2);
        }
    };
    let (plan_file, remaining) = match glomeris::cli::extract_plan_file(&after_roots) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris llm-plan: {e}");
            print_command_usage("llm-plan");
            std::process::exit(2);
        }
    };

    let mut json = false;
    let mut progress_json = false;
    let mut schema = false;
    let mut print_payload = false;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--json" => {
                json = true;
                i += 1;
            }
            "--progress-json" => {
                progress_json = true;
                i += 1;
            }
            "--schema" => {
                schema = true;
                i += 1;
            }
            "--print-payload" => {
                print_payload = true;
                i += 1;
            }
            other => {
                // Match on the flag name only — never the whole token — so a
                // `--api-key=<secret>` invocation can't echo the secret to
                // stderr the way printing `other`/`remaining[i]` verbatim
                // would. `credential_flag_name` isolates the flag name for
                // both the space-separated (`--api-key sk-...`) and
                // `=`-joined (`--api-key=sk-...`) forms without ever
                // touching the value.
                let flag_name = glomeris::cli::credential_flag_name(other);
                if matches!(flag_name, "--api-key" | "--key" | "--token") {
                    eprintln!(
                        "glomeris llm-plan: unrecognized argument '{flag_name}' — read the key \
                         from $GLOMERIS_LLM_API_KEY; passing a key in argv exposes it to ps and \
                         shell history"
                    );
                    std::process::exit(2);
                }
                eprintln!("glomeris llm-plan: unrecognized argument '{other}'");
                print_command_usage("llm-plan");
                std::process::exit(2);
            }
        }
    }

    if schema {
        println!("{}", glomeris::cli::llm_plan_schema_example());
        return;
    }

    let candidates = discover_and_classify_now_with_progress(project_roots, progress_json);
    let actions = ActionRegistry::builtin();

    // HORO-1298: prints the request and stops, before any provider is
    // constructed — so this needs no credential, makes no network call,
    // and is the same code path a live run would send, not a description
    // of it.
    if print_payload {
        match glomeris::cli::build_llm_payload_report(&candidates, &actions) {
            Ok(report) => {
                if json {
                    print_json_or_exit(&report);
                } else {
                    glomeris::cli::print_llm_payload_report(&report);
                }
            }
            Err(e) => {
                eprintln!("glomeris llm-plan: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Same free-space context `detect` uses, so an AI-suggested row's size
    // emphasis matches the identical row in the candidates list rather than
    // being computed against a different (or absent) notion of "free".
    let impact = impact_context();
    let report = match plan_file {
        Some(path) => {
            let provider = FilePlanProvider { path };
            glomeris::cli::build_llm_plan_report(&candidates, &actions, &provider, impact)
        }
        None => match provider_from_env() {
            Ok(provider) => {
                glomeris::cli::build_llm_plan_report(&candidates, &actions, &provider, impact)
            }
            // "You have not set this up" and "you set it up wrongly, here is
            // what to change" need different messages, and a user in the
            // second case is being actively misled by the first: they *did*
            // set all three variables. `Display` for
            // `InvalidConfiguration` carries only Glomeris's own guidance,
            // never the offending value.
            Err(glomeris::actions::llm::LlmError::InvalidConfiguration(detail)) => {
                eprintln!("glomeris llm-plan: {detail}");
                std::process::exit(2);
            }
            Err(_) => {
                eprintln!(
                    "glomeris llm-plan: missing LLM configuration — set GLOMERIS_LLM_API_KEY, \
                     GLOMERIS_LLM_BASE_URL, and GLOMERIS_LLM_MODEL, or pass \
                     --plan-file <path> instead"
                );
                std::process::exit(2);
            }
        },
    };

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_llm_plan_report(&report);
    }

    if report.provider_error.is_some() {
        std::process::exit(1);
    }
}

/// `glomeris llm-check [--json]` — does the configured BYOK setup work?
/// (HORO-1309)
///
/// The narrowest possible command: no project roots, no detectors, no
/// evidence, no policy, no actions. It sends the two fixed prompts in
/// [`glomeris::actions::llm::CONNECTION_TEST_SYSTEM_PROMPT`] through the same
/// [`glomeris::actions::llm::LlmProvider::complete`] a plan uses, so a pass
/// means a real request will work rather than meaning a cheaper probe
/// succeeded.
///
/// Exit codes, chosen so a GUI or script can branch without parsing output:
/// `0` the provider answered; `1` it did not (unreachable, rejected, or an
/// unusable response — the report says which, and is printed first); `2` the
/// configuration is absent or cannot work, i.e. nothing was sent and the fix
/// is local. Deliberately the same shape `llm-plan` uses: `2` means "fix your
/// invocation or setup", never "the network is having a bad day".
///
/// Like `llm-plan`, the credential comes only from `GLOMERIS_LLM_API_KEY` and
/// is never accepted as an argument — passing a key in argv exposes it to
/// `ps` and shell history.
fn run_llm_check_command(args: &[String]) {
    use glomeris::actions::llm::{
        chat_completions_endpoint_path, provider_from_env, LlmError, LlmProvider,
    };

    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => {
                // Flag NAME only, never the whole token — see the identical
                // guard in `run_llm_plan_command` for why printing `other`
                // verbatim would echo a `--api-key=<secret>` value.
                let flag_name = glomeris::cli::credential_flag_name(other);
                if matches!(flag_name, "--api-key" | "--key" | "--token") {
                    eprintln!(
                        "glomeris llm-check: unrecognized argument '{flag_name}' — read the key \
                         from $GLOMERIS_LLM_API_KEY; passing a key in argv exposes it to ps and \
                         shell history"
                    );
                    std::process::exit(2);
                }
                eprintln!("glomeris llm-check: unrecognized argument '{other}'");
                print_command_usage("llm-check");
                std::process::exit(2);
            }
        }
    }

    let provider = match provider_from_env() {
        Ok(provider) => provider,
        Err(LlmError::InvalidConfiguration(detail)) => {
            eprintln!("glomeris llm-check: {detail}");
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!(
                "glomeris llm-check: missing LLM configuration — set GLOMERIS_LLM_API_KEY, \
                 GLOMERIS_LLM_BASE_URL, and GLOMERIS_LLM_MODEL"
            );
            std::process::exit(2);
        }
    };

    // Read before the provider is moved behind `&dyn LlmProvider`, and by the
    // same rule `complete` builds its URL with — so the reported path is the
    // one actually posted to, not a second guess at it.
    let endpoint_path = chat_completions_endpoint_path(&provider.base_url);
    let model = provider.model.clone();
    let report = glomeris::cli::build_llm_check_report(
        &provider as &dyn LlmProvider,
        &model,
        &endpoint_path,
    );

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_llm_check_report(&report);
    }

    if report.outcome != "ok" {
        std::process::exit(1);
    }
}

/// `glomeris execute --action-id <id> --resource-id <id> [--project-root
/// <path>]... [--confirm-ask --observed-fingerprint <token>] [--json]
/// [--progress-json]` — the sole interactive destructive-execution
/// subcommand (HORO-1055).
///
/// The caller supplies ONLY selectors: a resource id, an action id, and
/// (for `Ask`) a previously-observed fingerprint token — never a
/// `PolicyClass`, a `PolicyDecision`, an `ActionPlan`, a raw path used as
/// a direct target, or any `--force`/override. This function's entire
/// job is argument parsing, the HORO-1054 execution lock, and mapping
/// [`glomeris::cli::ExecuteResolution`] to a typed exit code — every
/// actual enforcement decision happens in
/// [`glomeris::cli::resolve_and_execute`], which calls the real,
/// unmodified `policy::approval::authorize` and `executor::execute`.
#[cfg(target_os = "macos")]
fn run_execute_command(args: &[String]) {
    use glomeris::actions::ActionRegistry;
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::policy::PolicyConfig;
    use std::time::SystemTime;

    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris execute: {e}");
            print_command_usage("execute");
            std::process::exit(2);
        }
    };

    let mut action_id: Option<&str> = None;
    let mut resource_id: Option<&str> = None;
    let mut confirm_ask = false;
    let mut observed_fingerprint: Option<&str> = None;
    let mut json = false;
    let mut progress_json = false;

    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--action-id" => {
                action_id = remaining.get(i + 1).map(String::as_str);
                if action_id.is_none() {
                    eprintln!("glomeris execute: --action-id requires a value");
                    std::process::exit(2);
                }
                i += 2;
            }
            "--resource-id" => {
                resource_id = remaining.get(i + 1).map(String::as_str);
                if resource_id.is_none() {
                    eprintln!("glomeris execute: --resource-id requires a value");
                    std::process::exit(2);
                }
                i += 2;
            }
            "--confirm-ask" => {
                confirm_ask = true;
                i += 1;
            }
            "--observed-fingerprint" => {
                observed_fingerprint = remaining.get(i + 1).map(String::as_str);
                if observed_fingerprint.is_none() {
                    eprintln!("glomeris execute: --observed-fingerprint requires a value");
                    std::process::exit(2);
                }
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "--progress-json" => {
                progress_json = true;
                i += 1;
            }
            other => {
                eprintln!("glomeris execute: unrecognized argument '{other}'");
                print_command_usage("execute");
                std::process::exit(2);
            }
        }
    }

    let Some(action_id) = action_id else {
        eprintln!("glomeris execute: --action-id is required");
        print_command_usage("execute");
        std::process::exit(2);
    };
    let Some(resource_id) = resource_id else {
        eprintln!("glomeris execute: --resource-id is required");
        print_command_usage("execute");
        std::process::exit(2);
    };

    // `--confirm-ask` and `--observed-fingerprint` are a single unit: an
    // `Ask` approval always needs both together, never just one — passing
    // only one is ambiguous (which did the caller actually mean?) and is
    // rejected outright rather than guessed at.
    if confirm_ask != observed_fingerprint.is_some() {
        eprintln!(
            "glomeris execute: --confirm-ask and --observed-fingerprint must be passed together"
        );
        print_command_usage("execute");
        std::process::exit(2);
    }

    // Decode the caller-supplied token BEFORE ever touching the execution
    // lock or running discovery — a malformed token is a pure usage
    // error. This is the ONLY place a fingerprint is ever accepted from
    // the caller; there is no fallback to a freshly-observed fingerprint
    // anywhere in this command.
    let observed_fingerprint_from_token = match observed_fingerprint {
        Some(token) => match glomeris::evidence::decode_fingerprint_token(token) {
            Ok(fp) => Some(fp),
            Err(e) => {
                eprintln!("glomeris execute: invalid --observed-fingerprint token: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns.
    let _execution_lock = acquire_execution_lock_or_exit("execute", json);

    let candidates = discover_and_classify_now_with_progress(project_roots, progress_json);
    let actions = ActionRegistry::builtin();
    let collector = DefaultEvidenceCollector::default();
    let cfg = PolicyConfig::default();
    let now = SystemTime::now();

    let resolution = glomeris::cli::resolve_and_execute(
        &candidates,
        &actions,
        resource_id,
        action_id,
        observed_fingerprint_from_token,
        &collector,
        &cfg,
        now,
        &actions_jsonl_path(),
    );

    render_execute_resolution(resolution, json);
}

#[cfg(not(target_os = "macos"))]
fn run_execute_command(_args: &[String]) {
    eprintln!("glomeris execute: only supported on macOS");
    std::process::exit(1);
}

/// Maps one [`glomeris::cli::ExecuteResolution`] to `execute`'s typed exit
/// code (see `book/src/cli_reference.md`'s `execute` section for the full
/// table) and prints the corresponding report.
#[cfg(target_os = "macos")]
fn render_execute_resolution(resolution: glomeris::cli::ExecuteResolution, json: bool) {
    use glomeris::cli::ExecuteResolution;
    use glomeris::executor::ExecutionOutcome;
    use glomeris::reporting::dto::{ExecuteRefusalReport, RefusalReason};

    // Every refusal/not-found branch shares this shape: render as
    // structured JSON when --json is passed, else the existing
    // human-readable eprintln, then exit with `code`. --json callers
    // (the interactive UI this subcommand exists for) previously got
    // empty stdout plus a bare exit code here — exactly the cases a UI
    // most needs structured detail on.
    let refuse = |reason: RefusalReason, message: String, code: i32| -> ! {
        if json {
            print_json_or_exit(&ExecuteRefusalReport { reason, message });
        } else {
            eprintln!("glomeris execute: {message}");
        }
        std::process::exit(code);
    };

    match resolution {
        ExecuteResolution::ResourceNotFound => refuse(
            RefusalReason::ResourceNotFound,
            "no discoverable candidate matches the given --resource-id".to_string(),
            5,
        ),
        ExecuteResolution::ActionNotFound => refuse(
            RefusalReason::ActionNotFound,
            "no registered action resolves for this resource".to_string(),
            5,
        ),
        ExecuteResolution::ActionMismatch {
            requested,
            resolved,
        } => refuse(
            RefusalReason::ActionMismatch,
            format!(
                "--action-id '{requested}' does not match the action this resource actually \
                 resolves to ('{resolved}') — refusing to substitute a different action than \
                 the one requested"
            ),
            5,
        ),
        ExecuteResolution::RefusedProtected => refuse(
            RefusalReason::Protected,
            "refused — this resource is PROTECTED; no flag combination can authorize executing \
             against it"
                .to_string(),
            3,
        ),
        ExecuteResolution::RefusedAskNoConsent => refuse(
            RefusalReason::AskNoConsent,
            "refused — this resource requires confirmation (--confirm-ask plus a matching \
             --observed-fingerprint); none was supplied"
                .to_string(),
            3,
        ),
        ExecuteResolution::RefusedAskConsentMismatch => refuse(
            RefusalReason::AskConsentMismatch,
            "refused — the supplied --observed-fingerprint does not match this resource's \
             freshly observed identity (stale, or observed for a different resource)"
                .to_string(),
            3,
        ),
        ExecuteResolution::RefusedAutoSafeContractViolation => refuse(
            RefusalReason::AutoSafeContractViolation,
            "refused — an AUTO_SAFE decision failed to authorize, which contradicts \
             policy::approval::authorize's documented contract; refusing rather than proceeding"
                .to_string(),
            3,
        ),
        ExecuteResolution::Executed(report) => {
            let execute_report = glomeris::cli::build_execute_report(&report);
            if json {
                print_json_or_exit(&execute_report);
            } else {
                glomeris::cli::print_execute_report(&execute_report);
            }
            match &report.outcome {
                ExecutionOutcome::Succeeded => std::process::exit(0),
                ExecutionOutcome::Failed(_) => std::process::exit(1),
                ExecutionOutcome::AbortedByRevalidation(_) => std::process::exit(4),
                ExecutionOutcome::DryRun => std::process::exit(0),
            }
        }
    }
}

/// Default `--limit` for `glomeris history` (HORO-1046) when the caller
/// doesn't pass one — small enough to stay a quick glance, large enough to
/// span several recent transitions.
const DEFAULT_HISTORY_LIMIT: usize = 20;

/// Same `Library/Application Support/Glomeris/history.tsv` path
/// `daemon_run` writes to — shared here so `glomeris history` reads back
/// exactly what the poll loop recorded.
fn history_path() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/history.tsv"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-history.tsv"))
}

/// Same `Library/Application Support/Glomeris/` directory as
/// `history_path`/`heartbeat_path` (HORO-1057) — where every real-
/// execution call site (`execute`/`free`/`emergency`) appends its
/// best-effort audit trail, and `glomeris actions history` reads it back.
fn actions_jsonl_path() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/actions.jsonl"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-actions.jsonl"))
}

/// `glomeris history [--json] [--limit N]` — a bounded, oldest-first tail
/// of the monitor's `history.tsv` (HORO-1046). No new persistence format:
/// this reads the exact same TSV `FilePersistence::record` already writes.
/// A missing history file (daemon never ran, or never recorded a
/// transition) is not an error — it renders as an empty list, matching
/// `glomeris::monitor::read_history_tail`'s contract.
fn run_history_command(args: &[String]) {
    let mut limit = DEFAULT_HISTORY_LIMIT;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json = true;
                i += 1;
            }
            "--limit" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("glomeris history: --limit requires a value");
                    std::process::exit(2);
                };
                limit = match value.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("glomeris history: --limit must be a non-negative integer");
                        std::process::exit(2);
                    }
                };
                i += 2;
            }
            other => {
                eprintln!("glomeris history: unrecognized argument '{other}'");
                print_command_usage("history");
                std::process::exit(2);
            }
        }
    }

    let entries = glomeris::monitor::read_history_tail(&history_path(), limit);
    let report = glomeris::cli::build_history_report(&entries);

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_history_report(&report);
    }
}

/// How a `free` run came to be running, as the three combinations that exist
/// (HORO-1510).
///
/// A pair of bools would have a fourth state — unattended, with no envelope —
/// and that state is a run nobody asked for that nothing bounds, which is the
/// single thing this must not be able to express. The flag parser refuses it
/// once, at the boundary, and everything downstream is handed a value that
/// cannot mean it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunAuthority {
    /// Somebody ran this. The command line's own limits apply.
    Asked,
    /// Somebody ran this and asked for the standing grant's limits instead of
    /// the command line's.
    AskedWithinGrant,
    /// Nobody asked: started in answer to a disk-pressure alert. Requires a
    /// grant that authorizes starting unasked, not merely one that is enabled.
    UnpromptedWithinGrant,
}

impl RunAuthority {
    fn uses_envelope(self) -> bool {
        !matches!(self, RunAuthority::Asked)
    }

    fn is_unprompted(self) -> bool {
        matches!(self, RunAuthority::UnpromptedWithinGrant)
    }
}

/// `glomeris free` — the closed recovery loop, reachable on either axis
/// (HORO-1506).
///
/// Two goal flags, deliberately not aliases of each other:
///
/// - `--goal-used-percent <N>` is the product-facing form. It is target disk
///   *usage*, it is what the GUI sends, and it is validated against the
///   current reading so a goal that would reclaim nothing is refused rather
///   than run and reported as a success.
/// - `--target <N%|NB>` is the original raw *free-space floor*, unchanged in
///   both meaning and tolerance: a floor you already exceed stays a legitimate
///   no-op probe, which is exactly how `tests/execution_lock_wiring.rs` uses
///   it.
///
/// Exactly one is required. Accepting both would mean choosing a precedence
/// between two numbers that disagree about how much of a disk to delete.
///
/// `--autopilot` narrows the same loop with the stored Autopilot envelope
/// instead of starting a second one (HORO-1510). It changes nothing about how
/// the loop decides — it only withholds candidates the envelope does not cover
/// — so what it adds is entirely subtractive, and a run that stops because the
/// envelope ran out of allowance says so rather than reporting an exhausted
/// disk.
///
/// `--unattended` says nobody pressed anything: this run is the answer to a
/// disk-pressure alert. It is the caller's own statement about itself, and it
/// is here so that the authority check for starting unasked lives in this
/// binary rather than in whatever launched it. Without it, a client could start
/// an `--autopilot` run on its own initiative and nothing in Rust would be able
/// to tell that apart from a person typing the same command.
fn run_free_command(args: &[String]) {
    use glomeris::executor::goal::RecoveryGoal;

    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris free: {e}");
            print_command_usage("free");
            std::process::exit(2);
        }
    };

    let mut target_arg: Option<&str> = None;
    let mut goal_arg: Option<&str> = None;
    let mut dry_run = false;
    let mut json = false;
    let mut progress_json = false;
    let mut stop_file: Option<&str> = None;
    let mut autopilot = false;
    let mut unattended = false;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--autopilot" => {
                autopilot = true;
                i += 1;
            }
            "--unattended" => {
                unattended = true;
                i += 1;
            }
            "--target" => {
                target_arg = remaining.get(i + 1).map(String::as_str);
                i += 2;
            }
            "--goal-used-percent" => {
                goal_arg = remaining.get(i + 1).map(String::as_str);
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "--progress-json" => {
                progress_json = true;
                i += 1;
            }
            "--stop-file" => {
                stop_file = remaining.get(i + 1).map(String::as_str);
                i += 2;
            }
            other => {
                eprintln!("glomeris free: unrecognized argument '{other}'");
                print_command_usage("free");
                std::process::exit(2);
            }
        }
    }

    let (goal, target) = match (goal_arg, target_arg) {
        (Some(_), Some(_)) => {
            eprintln!(
                "glomeris free: --goal-used-percent and --target are two different axes \
                 (target usage vs. a free-space floor); pass exactly one"
            );
            print_command_usage("free");
            std::process::exit(2);
        }
        (None, None) => {
            eprintln!("glomeris free: one of --goal-used-percent or --target is required");
            print_command_usage("free");
            std::process::exit(2);
        }
        (Some(raw), None) => {
            let percent = match raw.trim().trim_end_matches('%').parse::<f64>() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!(
                        "glomeris free: --goal-used-percent must be a number between 0 and 100 \
                         (percent of disk USED), got '{raw}'"
                    );
                    std::process::exit(2);
                }
            };
            let goal = match RecoveryGoal::from_used_percent(percent) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("glomeris free: {e}");
                    std::process::exit(2);
                }
            };
            (Some(goal), goal.to_free_target())
        }
        (None, Some(raw)) => {
            let target = match glomeris::executor::recovery_loop::parse_free_target(raw) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("glomeris free: {e}");
                    std::process::exit(2);
                }
            };
            (None, target)
        }
    };

    // A dry run performs no actions, so there is nothing for a stop request to
    // stop. Accepting the flag and ignoring it would leave a caller believing it
    // had a handle on a run it does not — the same reason `--progress-json` was
    // refused on a real run until this one could honour it.
    if stop_file.is_some() && dry_run {
        eprintln!(
            "glomeris free: --stop-file asks a running loop to stop after its current \
             action; --dry-run performs no actions, so the two cannot be combined"
        );
        std::process::exit(2);
    }

    // Same reasoning as above, and the more important half of it: an envelope
    // is authority to execute. A preview executes nothing, so accepting the
    // flag here would produce a candidate list that looks envelope-filtered
    // while being nothing of the kind — a user would read it as "this is what
    // Autopilot would do" when it is the full list.
    if autopilot && dry_run {
        eprintln!(
            "glomeris free: --autopilot bounds what a run may execute; --dry-run executes \
             nothing, so the two cannot be combined. Use `glomeris autopilot show` to read \
             the envelope without running anything."
        );
        std::process::exit(2);
    }

    // The fourth state, refused here so that [`RunAuthority`] cannot carry it.
    // A run nobody asked for is exactly the one that must be bounded by a grant
    // somebody read, so the useful error is this one rather than a later
    // refusal by the envelope that would read as though the combination itself
    // were fine.
    if unattended && !autopilot {
        eprintln!(
            "glomeris free: --unattended says nobody asked for this run, so it may only run \
             inside the standing grant. Pass --autopilot as well, or drop --unattended."
        );
        print_command_usage("free");
        std::process::exit(2);
    }

    let authority = match (autopilot, unattended) {
        (false, _) => RunAuthority::Asked,
        (true, false) => RunAuthority::AskedWithinGrant,
        (true, true) => RunAuthority::UnpromptedWithinGrant,
    };

    if dry_run {
        free_preview(goal, target, project_roots, json, progress_json);
    } else {
        free_run(
            goal,
            target,
            project_roots,
            json,
            progress_json,
            stop_file.map(PathBuf::from),
            authority,
        );
    }
}

/// `glomeris autopilot [show|enable|revoke|run]` (HORO-1310).
///
/// Four verbs rather than a flag soup on one command, because the thing a
/// reader most needs certainty about here is which invocations can delete
/// something. `show` — the default, so a bare `glomeris autopilot` reads
/// rather than acts — and `revoke` never can. `enable` writes a grant and
/// performs no action. Only `run` executes, and only inside the grant that
/// `show` just printed.
fn run_autopilot_command(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("show");
    let rest: &[String] = if args.is_empty() { &[] } else { &args[1..] };

    match sub {
        "show" => autopilot_show(rest),
        "enable" => autopilot_enable(rest),
        "revoke" => autopilot_revoke(rest),
        "run" => autopilot_run(rest),
        other => {
            eprintln!("glomeris autopilot: unknown subcommand '{other}'");
            print_command_usage("autopilot");
            std::process::exit(2);
        }
    }
}

/// Exits 1 with the store's own message. One function so that a read
/// failure and a write failure cannot drift into different exit codes.
fn autopilot_store_error_exit(what: &str, e: glomeris::autopilot::StoreError) -> ! {
    eprintln!("glomeris autopilot: failed to {what} the envelope: {e}");
    std::process::exit(1);
}

fn autopilot_usage_exit(message: &str) -> ! {
    eprintln!("glomeris autopilot: {message}");
    print_command_usage("autopilot");
    std::process::exit(2);
}

fn autopilot_reject_extra_args(sub: &str, args: &[String]) {
    if let Some(unexpected) = args.first() {
        autopilot_usage_exit(&format!("{sub} takes no arguments (got '{unexpected}')"));
    }
}

/// Splits `--json` off a strict argument list, returning whether it was
/// present and whatever else was there.
///
/// Separate from [`split_flags`] because the verbs that use this reject
/// anything they do not recognize rather than collecting it, so the remainder
/// has to stay `String` for the existing rejection paths to keep reporting the
/// offending token. Shared by `autopilot`, `actions` and `settings`: three
/// commands whose `--json` means the same thing should not have three parsers
/// that could come to disagree about where the flag may appear.
fn take_json_flag(args: &[String]) -> (bool, Vec<String>) {
    let mut json = false;
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        if arg == "--json" {
            json = true;
        } else {
            rest.push(arg.clone());
        }
    }
    (json, rest)
}

/// Prints an envelope report as JSON, resolving the store path the same way
/// the human output does.
///
/// One function for all three verbs, because `show`, `enable` and `revoke`
/// differ only in what they did before reporting; a client that had to parse
/// three shapes to learn one thing would end up with three chances to be
/// wrong about it.
fn autopilot_print_json(envelope: &glomeris::autopilot::AutopilotEnvelope) {
    let stored_at = glomeris::autopilot::store::default_envelope_path()
        .ok()
        .map(|path| path.display().to_string());
    print_json_or_exit(&glomeris::autopilot::envelope_report(envelope, stored_at));
}

/// Reads the value that must follow `args[i]`, or exits 2.
fn autopilot_flag_value<'a>(args: &'a [String], i: usize, flag: &str) -> &'a str {
    match args.get(i + 1) {
        Some(value) => value.as_str(),
        None => autopilot_usage_exit(&format!("{flag} needs a value")),
    }
}

/// Prints the stored envelope. AC 6: what Autopilot is authorized to do,
/// readable before anything is enabled and without running anything.
fn autopilot_show(args: &[String]) {
    let (json, rest) = take_json_flag(args);
    autopilot_reject_extra_args("show", &rest);

    let envelope = match glomeris::autopilot::load_envelope() {
        Ok(envelope) => envelope,
        Err(e) => autopilot_store_error_exit("read", e),
    };

    if json {
        autopilot_print_json(&envelope);
        return;
    }

    for line in envelope.describe() {
        println!("{line}");
    }
    if let Ok(path) = glomeris::autopilot::store::default_envelope_path() {
        println!("stored at:         {}", path.display());
    }
}

/// `glomeris autopilot enable --kinds <tag,...> [limits]`.
///
/// Builds the new envelope from [`AutopilotEnvelope::revoked`] plus the
/// flags on *this* command line — deliberately not from whatever is already
/// on disk. An `enable` that accumulated onto the previous grant would make
/// the authority in force the union of every `enable` ever run, which is
/// precisely the thing nobody can hold in their head. One invocation states
/// the whole grant; anything unstated is the conservative default.
fn autopilot_enable(args: &[String]) {
    use glomeris::autopilot::AutopilotEnvelope;
    use glomeris::evidence::ResourceKind;
    use glomeris::monitor::PressureState;
    use glomeris::policy::ReasonCode;
    use std::time::Duration;

    let mut envelope = AutopilotEnvelope::revoked();
    let mut kinds_given = false;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--json" => {
                json = true;
                i += 1;
            }
            "--kinds" => {
                let raw = autopilot_flag_value(args, i, flag);
                for tag in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    let Some(kind) = ResourceKind::from_tag(tag) else {
                        let known: Vec<&str> = ResourceKind::ALL.iter().map(|k| k.tag()).collect();
                        autopilot_usage_exit(&format!(
                            "'{tag}' is not a resource kind. Known kinds: {}",
                            known.join(", ")
                        ));
                    };
                    if let Err(e) = envelope.allow_kind(kind) {
                        autopilot_usage_exit(&format!("--kinds {tag}: {e}"));
                    }
                    kinds_given = true;
                }
                i += 2;
            }
            "--max-actions" => {
                let raw = autopilot_flag_value(args, i, flag);
                let Ok(value) = raw.parse::<u32>() else {
                    autopilot_usage_exit(&format!("--max-actions {raw:?} is not a whole number"));
                };
                if let Err(e) = envelope.set_max_actions(value) {
                    autopilot_usage_exit(&format!("--max-actions: {e}"));
                }
                i += 2;
            }
            "--max-bytes" => {
                let raw = autopilot_flag_value(args, i, flag);
                let Ok(value) = raw.parse::<u64>() else {
                    autopilot_usage_exit(&format!("--max-bytes {raw:?} is not a whole number"));
                };
                if let Err(e) = envelope.set_max_bytes(value) {
                    autopilot_usage_exit(&format!("--max-bytes: {e}"));
                }
                i += 2;
            }
            "--max-duration" => {
                let raw = autopilot_flag_value(args, i, flag);
                let Ok(secs) = raw.parse::<u64>() else {
                    autopilot_usage_exit(&format!(
                        "--max-duration {raw:?} is not a whole number of seconds"
                    ));
                };
                if let Err(e) = envelope.set_max_duration(Duration::from_secs(secs)) {
                    autopilot_usage_exit(&format!("--max-duration: {e}"));
                }
                i += 2;
            }
            "--min-pressure" => {
                let raw = autopilot_flag_value(args, i, flag);
                let state = if raw.eq_ignore_ascii_case("none") {
                    None
                } else {
                    match PressureState::ALL
                        .iter()
                        .copied()
                        .find(|s| s.as_str().eq_ignore_ascii_case(raw))
                    {
                        Some(state) => Some(state),
                        None => {
                            let known: Vec<String> = PressureState::ALL
                                .iter()
                                .map(|s| s.as_str().to_lowercase())
                                .collect();
                            autopilot_usage_exit(&format!(
                                "--min-pressure {raw:?} is not a pressure state. \
                                 Known states: none, {}",
                                known.join(", ")
                            ));
                        }
                    }
                };
                envelope.set_min_pressure(state);
                i += 2;
            }
            "--preauthorize-ask" => {
                let raw = autopilot_flag_value(args, i, flag);
                let Some((kind_tag, reason_tag)) = raw.split_once(':') else {
                    autopilot_usage_exit(&format!(
                        "--preauthorize-ask {raw:?} must be <kind>:<reason>, \
                         e.g. node_modules:rebuild_cost_high"
                    ));
                };
                let Some(kind) = ResourceKind::from_tag(kind_tag.trim()) else {
                    autopilot_usage_exit(&format!(
                        "--preauthorize-ask: '{kind_tag}' is not a resource kind"
                    ));
                };
                let Some(reason) = ReasonCode::from_tag(reason_tag.trim()) else {
                    autopilot_usage_exit(&format!(
                        "--preauthorize-ask: '{reason_tag}' is not a policy reason code"
                    ));
                };
                // The envelope refuses any reason that is not pre-authorizable
                // — every PROTECTED reason, and every evidence-quality reason —
                // so a typo here cannot become a grant.
                if let Err(e) = envelope.preauthorize_ask(kind, reason) {
                    autopilot_usage_exit(&format!("--preauthorize-ask: {e}"));
                }
                i += 2;
            }
            // Its own flag rather than something `--min-pressure` implies,
            // because "only act once the disk is this bad" and "act without
            // being asked" are different grants and one of them is the one
            // that deletes things while nobody is looking (HORO-1510).
            "--respond-to-alerts" => {
                envelope.set_respond_to_alerts(true);
                i += 1;
            }
            other => autopilot_usage_exit(&format!("unrecognized argument '{other}'")),
        }
    }

    if !kinds_given {
        autopilot_usage_exit(
            "enable requires --kinds <tag,...>. An enabled envelope with an empty \
             allowlist can execute nothing, so it is a usage error rather than a \
             silent no-op",
        );
    }

    envelope.enable();
    if let Err(e) = glomeris::autopilot::save_envelope(&envelope) {
        autopilot_store_error_exit("write", e);
    }

    // Reported after the write, not before it: the point of `--json` here is
    // to tell a client what is now in force, and what is in force is what
    // reached the file.
    if json {
        autopilot_print_json(&envelope);
        return;
    }

    println!("Autopilot is now ENABLED, authorized to:");
    for line in envelope.describe() {
        println!("  {line}");
    }
    println!();
    println!("Revoke it at any time with `glomeris autopilot revoke`.");
    println!("See what it would do, without doing it: `glomeris autopilot run --dry-run`.");
}

/// `glomeris autopilot revoke` — AC 7. One bit, and every run re-reads the
/// file, so this takes effect on the next run with nothing to restart.
fn autopilot_revoke(args: &[String]) {
    let (json, rest) = take_json_flag(args);
    autopilot_reject_extra_args("revoke", &rest);

    let mut envelope = match glomeris::autopilot::load_envelope() {
        Ok(envelope) => envelope,
        Err(e) => autopilot_store_error_exit("read", e),
    };
    let was_enabled = envelope.is_enabled();
    envelope.revoke();
    if let Err(e) = glomeris::autopilot::save_envelope(&envelope) {
        autopilot_store_error_exit("write", e);
    }

    if json {
        autopilot_print_json(&envelope);
        return;
    }

    if was_enabled {
        println!("Autopilot revoked. No run can execute anything until it is enabled again.");
    } else {
        println!("Autopilot was already revoked. Nothing changed.");
    }
    // The limits survive revocation on purpose, so a later `enable` cannot
    // come back with limits the user never read. Printing them here is how
    // that stops being a surprise.
    for line in envelope.describe() {
        println!("  {line}");
    }
}

/// `glomeris autopilot run [--dry-run] [--plan-file <path>]
/// [--project-root <path>]...`
///
/// The only Autopilot verb that can delete. Every candidate it considers was
/// discovered by this process's own detectors, is re-classified by
/// [`glomeris::policy::classify`], must pass the envelope gate, must be
/// authorized through [`glomeris::policy::approval::authorize`], and is then
/// executed through the same TOCTOU revalidation every other deletion in
/// this binary goes through.
///
/// `--plan-file` is the only way a model's output reaches this command, and
/// it can do exactly one thing with it: change the order candidates are
/// considered in. There is deliberately no live-provider mode here — asking
/// a provider is `llm-plan`'s job, and keeping the network out of the
/// executing command means no deletion in this product can be blocked on, or
/// hurried by, a network call.
#[cfg(target_os = "macos")]
fn autopilot_run(args: &[String]) {
    use glomeris::actions::llm::{plan_with_llm, FilePlanProvider, ValidatedPlanItem};
    use glomeris::actions::ActionRegistry;
    use glomeris::autopilot::{
        load_envelope, run_autopilot, AutopilotItemOutcome, AutopilotRunRequest,
    };
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::monitor::{FsStat, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;
    use glomeris::policy::PolicyConfig;
    use std::time::{Instant, SystemTime};

    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => autopilot_usage_exit(&format!("run: {e}")),
    };

    let mut dry_run = false;
    let mut plan_file: Option<PathBuf> = None;
    let mut i = 0;
    while i < remaining.len() {
        match remaining[i].as_str() {
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--plan-file" => {
                plan_file = Some(PathBuf::from(autopilot_flag_value(
                    &remaining,
                    i,
                    "--plan-file",
                )));
                i += 2;
            }
            other => autopilot_usage_exit(&format!("run: unrecognized argument '{other}'")),
        }
    }

    let envelope = match load_envelope() {
        Ok(envelope) => envelope,
        Err(e) => autopilot_store_error_exit("read", e),
    };

    // The grant is printed before anything runs, so the output of a real run
    // opens with the authority it acted under rather than asking the reader
    // to go and look it up afterwards.
    println!("Autopilot envelope:");
    for line in envelope.describe() {
        println!("  {line}");
    }
    println!();

    if !envelope.is_enabled() {
        println!("Autopilot is not enabled. Nothing was attempted.");
        println!("Enable a narrow envelope first, e.g.");
        println!("  glomeris autopilot enable --kinds node_modules --max-actions 1");
        std::process::exit(3);
    }

    // The lock is held only by a run that can delete. A dry run mutates
    // nothing, so it has no business blocking a real `free` in another
    // terminal.
    let _execution_lock = if dry_run {
        None
    } else {
        Some(acquire_execution_lock_or_exit("autopilot", false))
    };

    // The discovery-time decision is kept, not discarded: `plan_with_llm`
    // needs it to decide which action ids may honestly be offered to a
    // model (HORO-1360). It is NOT what authorizes anything — Autopilot
    // re-runs `classify` on freshly correlated evidence for every candidate
    // it considers, and that decision is the only one execution reads.
    let classified: Vec<(
        glomeris::evidence::Evidence,
        glomeris::policy::PolicyDecision,
    )> = discover_and_classify_now(project_roots.clone());
    let candidates: Vec<glomeris::evidence::Evidence> = classified
        .iter()
        .map(|(evidence, _decision)| evidence.clone())
        .collect();

    let actions = ActionRegistry::builtin();

    let model_order: Vec<ValidatedPlanItem> = match &plan_file {
        None => Vec::new(),
        Some(path) => {
            let result = plan_with_llm(
                &FilePlanProvider { path: path.clone() },
                &classified,
                &actions,
            );
            // A plan that could not be read or parsed is not a failure of the
            // run: ordering falls back to local discovery order, which is
            // exactly what a run with no plan file does. Said out loud rather
            // than swallowed, because "my plan was ignored" is otherwise
            // indistinguishable from "my plan was followed".
            if let Some(e) = &result.provider_error {
                eprintln!(
                    "glomeris autopilot run: the plan file was not usable ({e}) — \
                     continuing in local discovery order"
                );
            }
            if result.dropped_unknown_resource > 0 || result.dropped_unknown_action > 0 {
                eprintln!(
                    "glomeris autopilot run: dropped {} plan item(s) naming a resource \
                     this machine does not have, and {} naming an action this binary \
                     does not register",
                    result.dropped_unknown_resource, result.dropped_unknown_action
                );
            }
            result.validated_items
        }
    };

    let observed_pressure = match MacosFsStat.stat(std::path::Path::new("/")) {
        Ok(usage) => {
            Some(ThresholdConfig::default().classify(usage.used_percent(), usage.free_bytes))
        }
        // Unobserved, not "fine": the gate treats a missing observation as
        // failing any configured pressure floor.
        Err(e) => {
            eprintln!(
                "glomeris autopilot run: could not read disk usage ({e}) — \
                 pressure is unobserved"
            );
            None
        }
    };

    let collector = DefaultEvidenceCollector::default();
    let policy = PolicyConfig::default();

    let report = run_autopilot(AutopilotRunRequest {
        envelope: &envelope,
        candidates,
        model_order: &model_order,
        collector: &collector,
        actions: &actions,
        policy: &policy,
        observed_pressure,
        now: SystemTime::now(),
        started_at: Instant::now(),
        audit_log_path: &actions_jsonl_path(),
        dry_run,
    });

    print!("{report}");

    if report
        .items
        .iter()
        .any(|item| matches!(item.outcome, AutopilotItemOutcome::Failed(_)))
    {
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn autopilot_run(_args: &[String]) {
    eprintln!("glomeris autopilot run: only supported on macOS");
    std::process::exit(1);
}

/// Print a finished recovery run as prose.
///
/// `goal`/`target` are printed because the report alone does not say what the
/// run was aiming at, and a free-space figure with no stated goal is the
/// ambiguity HORO-1506 exists to remove. The stop reason is printed as a
/// sentence from `stop_reason_detail` as well as its stable tag: a bare
/// `safe_exhausted` does not tell a user that the goal was *not* reached.
///
/// The tag comes from `stop_reason_tag` rather than `Debug`, so this prints the
/// same word the JSON does; `SafeExhausted`'s own breakdown gets a line of its
/// own below instead of arriving as a derived struct dump (HORO-1509).
fn print_recovery_report(
    goal: Option<&glomeris::executor::goal::RecoveryGoal>,
    target: &glomeris::executor::recovery_loop::FreeTarget,
    report: &glomeris::executor::recovery_loop::RecoveryReport,
) {
    use glomeris::cli::recovery::{describe_free_target, stop_reason_detail};
    use glomeris::executor::recovery_loop::StopReason;
    use glomeris::reporting::dto::stop_reason_tag;
    use glomeris::reporting::human_bytes;

    match goal {
        Some(goal) => println!("recovery goal:          {}", goal.describe()),
        None => println!("free-space target:      {}", describe_free_target(target)),
    }
    println!(
        "stop reason:            {}",
        stop_reason_tag(&report.stop_reason)
    );
    println!(
        "                        {}",
        stop_reason_detail(&report.stop_reason)
    );
    // Printed only for the one stop reason that concluded something about what
    // it left alone. Each count is a different next step for the user — say
    // yes, wait for the tool to finish, or nothing at all — so they are listed
    // separately rather than summed, and they are counts of candidates rather
    // than bytes: space this run was not permitted to take is not an
    // opportunity (HORO-1509).
    if let StopReason::SafeExhausted(remaining) = &report.stop_reason {
        println!(
            "still there:            {} awaiting your confirmation, {} not runnable now, \
             {} protected",
            remaining.requires_confirmation, remaining.not_executable, remaining.protected
        );
    }
    println!("iterations run:         {}", report.iterations_run);
    println!("actions executed:       {}", report.actions_executed);
    println!(
        "actions declined/skipped: {}",
        report.actions_declined_or_skipped
    );
    // `total_bytes_freed` is a MEASURED total (see
    // `RecoveryReport::total_bytes_freed`'s own doc comment: "Sum of
    // ACTUAL (not expected) reclaimed bytes"), never an estimate — labeled
    // explicitly as such so this never reads as the same kind of number as
    // a detector's `reclaimable_bytes` estimate.
    println!(
        "bytes freed (measured): {} ({} bytes)",
        human_bytes(report.total_bytes_freed),
        report.total_bytes_freed
    );
    println!(
        "free before:            {} ({} bytes)",
        human_bytes(report.started_free_bytes),
        report.started_free_bytes
    );
    println!(
        "free after:             {} ({} bytes)",
        human_bytes(report.final_free_bytes),
        report.final_free_bytes
    );

    // A detector that FAILED did not answer, so whatever it would have found
    // is unknown. Reporting the run without saying so lets a partial search
    // read as a complete one — and when the run stopped at `SafeExhausted`
    // that is an actively wrong claim, because "no safe candidate remains"
    // was concluded without having looked everywhere (HORO-1484). The wording
    // lives on the report so it has one producer and is testable.
    for line in report.discovery_caveat_lines() {
        println!("{line}");
    }
}

/// `glomeris pressure [show|notified|respond <answer>]` (HORO-1508).
///
/// The seam between the daemon and the menu-bar app. `UNUserNotificationCenter`
/// is the only macOS API that can put buttons on a notification and it refuses
/// to run outside an app bundle, while the process that notices pressure is a
/// bare launchd job. So the daemon records that a notification is *owed* and the
/// app raises it — which needs a way to ask what is owed, and a way to say what
/// the user pressed.
///
/// Every rule stays here. The app learns whether a banner is due by reading
/// `notification_due`, not by comparing a percentage against a threshold, and it
/// reports the user's answer as one of exactly three tokens this command
/// publishes. There is deliberately no way to express a fourth.
///
/// `show` is the default and is read-only. It does not call
/// [`glomeris::monitor::EpisodeTracker::observe`], and that is the distinction
/// this command rests on: the daemon is the only observer. If `show` opened
/// episodes, a machine whose daemon was never installed would still notify from
/// whatever process happened to run the GUI, and "the daemon owns pressure
/// policy" would stop being true. The honest consequence — no daemon means no
/// episode — is visible rather than hidden: `threshold_crossed` still reports
/// that the disk is above the user's threshold, and `daemon status` already says
/// whether anything is watching.
///
/// Nothing here deletes anything. An episode decides when the user is spoken to,
/// never what may be removed.
#[cfg(target_os = "macos")]
fn run_pressure_command(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("show");
    let rest: &[String] = if args.is_empty() { &[] } else { &args[1..] };

    match sub {
        "show" => pressure_show(rest),
        "notified" => pressure_notified(rest),
        "respond" => pressure_respond(rest),
        other => {
            eprintln!("glomeris pressure: unknown subcommand '{other}'");
            print_command_usage("pressure");
            std::process::exit(2);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn run_pressure_command(_args: &[String]) {
    eprintln!("glomeris pressure: only supported on macOS");
    std::process::exit(1);
}

/// Exit code for an answer or acknowledgement there was nothing to record.
///
/// Not 1, because nothing failed, and not 2, because the command line was
/// well-formed. `execute` established 3 as "refused, not broken" and this is the
/// same meaning: the ordinary cause is benign — the disk recovered between the
/// banner appearing and the button being pressed — and a caller that treated it
/// as a malfunction would report a fault to the user for a race that resolved
/// itself correctly.
#[cfg(target_os = "macos")]
const EXIT_PRESSURE_NOTHING_TO_RECORD: i32 = 3;

#[cfg(target_os = "macos")]
fn pressure_usage_exit(message: &str) -> ! {
    eprintln!("glomeris pressure: {message}");
    print_command_usage("pressure");
    std::process::exit(2);
}

/// Resolves the episode state file, exiting 1 if there is no `HOME` to resolve
/// it against.
///
/// Unlike a *missing* file — which [`glomeris::monitor::load_tracker_at`]
/// deliberately answers with a fresh tracker, because that is what a first run
/// looks like — an unresolvable path means this process cannot find the state at
/// all, and reporting "no episode" would be a guess dressed as an answer.
#[cfg(target_os = "macos")]
fn pressure_state_path() -> PathBuf {
    match glomeris::monitor::default_episode_state_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("glomeris pressure: failed to locate the episode state file: {e}");
            std::process::exit(1);
        }
    }
}

/// Loads the user's settings, or exits 1.
///
/// The threshold comes from [`glomeris::settings`] rather than from the stored
/// episode for the reason [`glomeris::monitor::episode_store`] documents: a
/// threshold the user changed while the daemon was down must govern now.
#[cfg(target_os = "macos")]
fn pressure_settings() -> glomeris::settings::RecoverySettings {
    match glomeris::settings::load_settings() {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("glomeris pressure: failed to read your settings: {e}");
            std::process::exit(1);
        }
    }
}

/// Builds and prints the current pressure status.
///
/// One function for all three verbs, so the app cannot be reading one shape
/// after `show` and another after `respond` — the same arrangement, for the same
/// reason, as [`settings_print_json`].
#[cfg(target_os = "macos")]
fn pressure_print_status(
    json: bool,
    tracker: &glomeris::monitor::EpisodeTracker,
    settings: &glomeris::settings::RecoverySettings,
    state_path: &std::path::Path,
) {
    use glomeris::monitor::{FsStat, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;

    let usage = match MacosFsStat.stat(std::path::Path::new("/")) {
        Ok(usage) => usage,
        Err(e) => {
            eprintln!("glomeris pressure: failed to read filesystem usage: {e}");
            std::process::exit(1);
        }
    };
    let report = glomeris::cli::pressure::build_pressure_status_report(
        tracker,
        settings,
        &usage,
        &ThresholdConfig::default(),
        glomeris::monitor::persistence::unix_now_secs(),
        Some(state_path.display().to_string()),
    );

    if json {
        print_json_or_exit(&report);
    } else {
        for line in glomeris::cli::pressure::describe_pressure_status(&report) {
            println!("{line}");
        }
    }
}

/// Reports a refusal and exits 3.
///
/// The refusal is printed as the same `reason`/`message` pair in both modes, so
/// a script branching on the token and a human reading the line are told the
/// same thing by the same producer.
#[cfg(target_os = "macos")]
fn pressure_rejection_exit(json: bool, rejection: glomeris::monitor::EpisodeRejection) -> ! {
    let report = glomeris::cli::pressure::build_pressure_rejection_report(&rejection);
    if json {
        print_json_or_exit(&report);
    } else {
        eprintln!("glomeris pressure: {}", report.message);
    }
    std::process::exit(EXIT_PRESSURE_NOTHING_TO_RECORD);
}

/// Loads the tracker under the current settings' threshold.
#[cfg(target_os = "macos")]
fn pressure_load_tracker(
    settings: &glomeris::settings::RecoverySettings,
    state_path: &std::path::Path,
) -> glomeris::monitor::EpisodeTracker {
    let config = glomeris::monitor::EpisodeConfig::new(settings.notify_at_used_percent());
    glomeris::monitor::load_tracker_at(state_path, config)
}

/// Persists the tracker, or exits 1.
///
/// A write failure is reported rather than swallowed, because the whole point of
/// both writing verbs is that the record survives: an acknowledgement that
/// silently failed to persist would have the app raise the same banner again on
/// the next poll, which is precisely the storm AC 5 forbids.
#[cfg(target_os = "macos")]
fn pressure_save_tracker(
    state_path: &std::path::Path,
    tracker: &glomeris::monitor::EpisodeTracker,
) {
    if let Err(e) = glomeris::monitor::save_tracker_at(state_path, tracker) {
        eprintln!("glomeris pressure: failed to record the episode state: {e}");
        std::process::exit(1);
    }
}

/// `glomeris pressure show [--json]` — what the app reads to decide whether to
/// raise a banner.
#[cfg(target_os = "macos")]
fn pressure_show(args: &[String]) {
    let (json, rest) = take_json_flag(args);
    if let Some(unexpected) = rest.first() {
        pressure_usage_exit(&format!("show takes no arguments (got '{unexpected}')"));
    }

    let settings = pressure_settings();
    let state_path = pressure_state_path();
    let tracker = pressure_load_tracker(&settings, &state_path);
    pressure_print_status(json, &tracker, &settings, &state_path);
}

/// `glomeris pressure notified [--json]` — the app reporting that it put the
/// banner on screen.
///
/// Separate from `respond` because they answer different questions and only one
/// of them ever happens: a user who ignores a banner entirely never responds,
/// and without this verb the episode would look un-notified forever and be
/// re-raised at every poll. It is also what makes `notifications_raised` mean
/// "banners the user could have seen" rather than "banners we intended", which
/// is the figure AC 5 is checked against.
#[cfg(target_os = "macos")]
fn pressure_notified(args: &[String]) {
    let (json, rest) = take_json_flag(args);
    if let Some(unexpected) = rest.first() {
        pressure_usage_exit(&format!("notified takes no arguments (got '{unexpected}')"));
    }

    let settings = pressure_settings();
    let state_path = pressure_state_path();
    let mut tracker = pressure_load_tracker(&settings, &state_path);
    if let Err(rejection) = tracker.mark_notified(glomeris::monitor::persistence::unix_now_secs()) {
        pressure_rejection_exit(json, rejection);
    }
    pressure_save_tracker(&state_path, &tracker);
    pressure_print_status(json, &tracker, &settings, &state_path);
}

/// `glomeris pressure respond <answer> [--json]` — the app reporting which
/// button the user pressed.
///
/// The answer is parsed through [`glomeris::monitor::EpisodeResponse::parse`],
/// so the accepted set is the domain type's and an unknown token is a usage
/// error listing what exists rather than a silently discarded answer. The three
/// tokens are also published in `pressure show`'s `responses` field, so a client
/// never has to know them independently — which is what stops it from offering a
/// fourth button and discovering the refusal only after the user pressed it.
#[cfg(target_os = "macos")]
fn pressure_respond(args: &[String]) {
    use glomeris::monitor::EpisodeResponse;

    let (json, rest) = take_json_flag(args);
    let raw = match rest.first() {
        Some(answer) => answer.as_str(),
        None => pressure_usage_exit(&format!(
            "respond needs an answer ({})",
            pressure_answer_list()
        )),
    };
    if let Some(unexpected) = rest.get(1) {
        pressure_usage_exit(&format!("respond takes one answer (got '{unexpected}')"));
    }
    let response = match EpisodeResponse::parse(raw) {
        Some(response) => response,
        None => pressure_usage_exit(&format!(
            "unknown answer '{raw}' ({})",
            pressure_answer_list()
        )),
    };

    let settings = pressure_settings();
    let state_path = pressure_state_path();
    let mut tracker = pressure_load_tracker(&settings, &state_path);
    if let Err(rejection) =
        tracker.respond(response, glomeris::monitor::persistence::unix_now_secs())
    {
        pressure_rejection_exit(json, rejection);
    }
    pressure_save_tracker(&state_path, &tracker);
    pressure_print_status(json, &tracker, &settings, &state_path);
}

/// The accepted answers, rendered from the enum rather than written out, so a
/// fourth response cannot be added without this message learning about it.
#[cfg(target_os = "macos")]
fn pressure_answer_list() -> String {
    let answers: Vec<&str> = glomeris::monitor::EpisodeResponse::ALL
        .iter()
        .map(|response| response.as_str())
        .collect();
    format!("one of: {}", answers.join(", "))
}

/// `glomeris settings [show|set]` (HORO-1507).
///
/// `show` is the default, so a bare `glomeris settings` reads rather than
/// writes — the same choice `autopilot` makes, and for the same reason: the
/// command whose name is a noun should answer a question.
///
/// Nothing here deletes anything, and nothing here can widen what policy
/// permits. The two numbers this command stores decide when the product
/// speaks up and where recovery stops; every gate that stands between a
/// candidate and its deletion is somewhere else entirely.
fn run_settings_command(args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("show");
    let rest: &[String] = if args.is_empty() { &[] } else { &args[1..] };

    match sub {
        "show" => settings_show(rest),
        "set" => settings_set(rest),
        other => {
            eprintln!("glomeris settings: unknown subcommand '{other}'");
            print_command_usage("settings");
            std::process::exit(2);
        }
    }
}

fn settings_usage_exit(message: &str) -> ! {
    eprintln!("glomeris settings: {message}");
    print_command_usage("settings");
    std::process::exit(2);
}

/// Exits 1 with the store's own message. One function so that a read failure
/// and a write failure cannot drift into different exit codes — the same
/// arrangement as [`autopilot_store_error_exit`].
///
/// Every store error lands here, including a `Refused` one. A stored file whose
/// two numbers contradict each other is not this command line's mistake, and
/// reporting it as a usage error would tell a script "you typed something
/// wrong" about an invocation that typed nothing. Exit 2 is reserved for values
/// that arrived as arguments.
fn settings_store_error_exit(what: &str, e: glomeris::settings::SettingsStoreError) -> ! {
    eprintln!("glomeris settings: failed to {what} your settings: {e}");
    std::process::exit(1);
}

/// Prints settings as JSON, resolving the store path the same way the human
/// output does.
///
/// One function for both verbs, so a client that reads `settings show` and a
/// client that reads the result of `settings set` cannot be looking at two
/// different shapes of the same two numbers.
fn settings_print_json(settings: &glomeris::settings::RecoverySettings, loaded_from_file: bool) {
    let stored_at = glomeris::settings::default_settings_path()
        .ok()
        .map(|path| path.display().to_string());
    print_json_or_exit(&glomeris::cli::settings::build_settings_report(
        settings,
        stored_at,
        loaded_from_file,
    ));
}

fn settings_print_human(settings: &glomeris::settings::RecoverySettings, loaded_from_file: bool) {
    for line in glomeris::cli::settings::describe_settings(settings) {
        println!("{line}");
    }
    if let Ok(path) = glomeris::settings::default_settings_path() {
        println!("stored at:         {}", path.display());
    }
    if !loaded_from_file {
        // Said plainly, because "75% used" looks identical whether the user
        // chose it or the product did, and only one of those is a decision
        // anybody made.
        println!("(built-in defaults — nothing stored yet)");
    }
}

/// Whether a settings file exists at the default path.
///
/// Asked separately from loading rather than reported by the loader, because
/// [`glomeris::settings::load_settings`] deliberately answers a missing file
/// with the defaults — that is what makes a first run work. The GUI still
/// needs to know which of the two happened.
fn settings_file_exists() -> bool {
    glomeris::settings::default_settings_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

fn settings_show(args: &[String]) {
    let (json, rest) = take_json_flag(args);
    if let Some(unexpected) = rest.first() {
        settings_usage_exit(&format!("show takes no arguments (got '{unexpected}')"));
    }

    let stored = settings_file_exists();
    let settings = match glomeris::settings::load_settings() {
        Ok(settings) => settings,
        Err(e) => settings_store_error_exit("read", e),
    };

    if json {
        settings_print_json(&settings, stored);
    } else {
        settings_print_human(&settings, stored);
    }
}

/// `glomeris settings set [--notify-at-used-percent N] [--default-goal-used-percent M]`.
///
/// Both flags are applied in one call to
/// [`glomeris::settings::RecoverySettings::with_changes`] rather than one after
/// the other, and that is load-bearing rather than tidy. A goal must stay below
/// the threshold, so moving a pair from (75, 70) down to (60, 55) is a valid
/// destination that is unreachable one field at a time: whichever field moves
/// first is momentarily invalid against the old value of the other. Validating
/// the pair once means the user is refused for the destination they asked for,
/// never for the order the flags happened to appear in.
///
/// Requires at least one flag. `set` with neither is not an expensive no-op to
/// be tolerated — it is a command line that did not say what it wanted, and
/// silently succeeding at nothing is how a typo becomes "I changed it and it
/// didn't work".
fn settings_set(args: &[String]) {
    let (json, rest) = take_json_flag(args);

    let mut notify_at: Option<f64> = None;
    let mut goal: Option<f64> = None;
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        match flag {
            "--notify-at-used-percent" | "--default-goal-used-percent" => {
                let raw = match rest.get(i + 1) {
                    Some(value) => value.as_str(),
                    None => settings_usage_exit(&format!("{flag} needs a value")),
                };
                // The percent sign is accepted here for the same reason the
                // config parser accepts it: a user who types what they read on
                // screen has not made a mistake.
                let value: f64 = match raw.trim_end_matches('%').parse() {
                    Ok(value) => value,
                    Err(_) => settings_usage_exit(&format!("{flag} needs a number (got '{raw}')")),
                };
                let slot = if flag == "--notify-at-used-percent" {
                    &mut notify_at
                } else {
                    &mut goal
                };
                if slot.is_some() {
                    settings_usage_exit(&format!("{flag} was given twice"));
                }
                *slot = Some(value);
                i += 2;
            }
            other => settings_usage_exit(&format!("unknown option '{other}'")),
        }
    }

    if notify_at.is_none() && goal.is_none() {
        settings_usage_exit(
            "set needs --notify-at-used-percent, --default-goal-used-percent, or both",
        );
    }

    let current = match glomeris::settings::load_settings() {
        Ok(settings) => settings,
        Err(e) => settings_store_error_exit("read", e),
    };
    let updated = match current.with_changes(notify_at, goal) {
        Ok(settings) => settings,
        Err(rejection) => {
            if json {
                print_json_or_exit(&glomeris::cli::settings::build_settings_rejection_report(
                    &rejection,
                ));
            } else {
                eprintln!("glomeris settings: {rejection}");
            }
            std::process::exit(2);
        }
    };
    if let Err(e) = glomeris::settings::save_settings(&updated) {
        settings_store_error_exit("write", e);
    }

    // Reported as stored, not as requested: after a successful write the file
    // exists, so `loaded_from_file` is true even on the run that created it.
    if json {
        settings_print_json(&updated, true);
    } else {
        settings_print_human(&updated, true);
    }
}

fn run_daemon_command(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("install") => {
            let mut force = false;
            for arg in &args[1..] {
                if arg == "--force" {
                    force = true;
                } else {
                    eprintln!("glomeris daemon install: unrecognized argument '{arg}'");
                    std::process::exit(2);
                }
            }
            daemon_install(force);
        }
        Some("uninstall") => daemon_uninstall(),
        Some("status") => daemon_status(&args[1..]),
        Some("run") => daemon_run(),
        Some(other) => {
            eprintln!("glomeris daemon: unknown subcommand '{other}'");
            print_command_usage("daemon");
            std::process::exit(2);
        }
        None => {
            print_command_usage("daemon");
            std::process::exit(2);
        }
    }
}

/// `glomeris actions <list [--json]|history [--json] [--limit <N>]>`
/// (HORO-1047, HORO-1057) — origin: v0.2.0 founder-dogfood had to read
/// `src/actions/homebrew.rs` source directly to find the real registered
/// action id string (`homebrew.cleanup.cache`); no command exposed the
/// registry. `list` is pure enumeration of `ActionRegistry::builtin()`;
/// `history` reads back `actions.jsonl`, the real-execution audit trail
/// `execute`/`free`/`emergency` append to. Neither subcommand is gated to
/// macOS, unlike `daemon` — `list` touches no filesystem/launchd state at
/// all, and `history` only reads a plain file (missing is not an error,
/// per `read_audit_tail`'s contract), same reasoning as `glomeris
/// history`.
fn run_actions_command(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("list") => actions_list(&args[1..]),
        Some("history") => actions_history(&args[1..]),
        Some(other) => {
            eprintln!("glomeris actions: unknown subcommand '{other}'");
            print_command_usage("actions");
            std::process::exit(2);
        }
        None => {
            print_command_usage("actions");
            std::process::exit(2);
        }
    }
}

/// Default `--limit` for `glomeris actions history` (HORO-1057) — same
/// value as `glomeris history`'s `DEFAULT_HISTORY_LIMIT`, for the same
/// reasoning: small enough to stay a quick glance, large enough to span
/// several recent actions.
const DEFAULT_ACTION_HISTORY_LIMIT: usize = 20;

/// `glomeris actions history [--json] [--limit N]` — a bounded,
/// oldest-first tail of `actions.jsonl` (HORO-1057). No new persistence
/// format beyond what `append_audit_record` already writes. A missing
/// `actions.jsonl` (no real execution has ever run) is not an error — it
/// renders as an empty list, matching
/// `glomeris::monitor::read_audit_tail`'s contract, same as `glomeris
/// history`'s handling of a missing `history.tsv`.
fn actions_history(args: &[String]) {
    let mut limit = DEFAULT_ACTION_HISTORY_LIMIT;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json = true;
                i += 1;
            }
            "--limit" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("glomeris actions history: --limit requires a value");
                    std::process::exit(2);
                };
                limit = match value.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!(
                            "glomeris actions history: --limit must be a non-negative integer"
                        );
                        std::process::exit(2);
                    }
                };
                i += 2;
            }
            other => {
                eprintln!("glomeris actions history: unrecognized argument '{other}'");
                print_command_usage("actions");
                std::process::exit(2);
            }
        }
    }

    let records = glomeris::monitor::read_audit_tail(&actions_jsonl_path(), limit);
    let report = glomeris::cli::build_action_history_report(&records);

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_action_history_report(&report);
    }
}

fn actions_list(args: &[String]) {
    let flags = flags_only("actions list", args, &["--json"]);

    let registry = glomeris::actions::ActionRegistry::builtin();
    let report = glomeris::cli::build_action_list_report(&registry);

    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_action_list_report(&report);
    }
}

#[cfg(target_os = "macos")]
fn current_exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("glomeris"))
}

#[cfg(target_os = "macos")]
fn daemon_install(force: bool) {
    use platform::macos::launchd::InstallOutcome;

    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon install: {e}");
            std::process::exit(1);
        }
    };
    match platform::macos::launchd::install(&plist_path, &current_exe_path(), force) {
        Ok(InstallOutcome::Installed) => {
            println!("installed launch agent at {}", plist_path.display());
        }
        Ok(InstallOutcome::AlreadyUpToDate) => {
            println!(
                "launch agent already up to date at {}",
                plist_path.display()
            );
        }
        Ok(InstallOutcome::Repaired) => {
            println!(
                "repaired launch agent at {} (preserved existing StartInterval)",
                plist_path.display()
            );
        }
        Ok(InstallOutcome::RefusedNeedsForce) => {
            eprintln!(
                "refused: existing plist at {} has unrecognized customization — rerun with \
                 --force to overwrite (a backup will be taken first)",
                plist_path.display()
            );
            std::process::exit(2);
        }
        Ok(InstallOutcome::AbortedConcurrentModification) => {
            eprintln!(
                "aborted: {} was modified concurrently, no changes made — retry",
                plist_path.display()
            );
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("glomeris daemon install: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "macos")]
fn daemon_uninstall() {
    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon uninstall: {e}");
            std::process::exit(1);
        }
    };
    match platform::macos::launchd::uninstall(&plist_path) {
        Ok(()) => println!("uninstalled launch agent"),
        Err(e) => {
            eprintln!("glomeris daemon uninstall: {e}");
            std::process::exit(1);
        }
    }
}

/// Same `Library/Application Support/Glomeris/heartbeat.json` path
/// `daemon_run` writes to (HORO-1044) — shared here so `daemon status
/// --json` (HORO-1045) reads back exactly what the poll loop wrote.
#[cfg(target_os = "macos")]
fn heartbeat_path() -> PathBuf {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/heartbeat.json"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-heartbeat.json"))
}

#[cfg(target_os = "macos")]
fn daemon_status(args: &[String]) {
    let flags = flags_only("daemon status", args, &["--json"]);

    let plist_path = match platform::macos::launchd::default_plist_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("glomeris daemon status: {e}");
            std::process::exit(1);
        }
    };
    let status = platform::macos::launchd::status(&plist_path);
    let heartbeat = monitor::read_heartbeat(&heartbeat_path());
    let now = monitor::persistence::unix_now_secs();

    let report = glomeris::cli::build_daemon_status_report(
        status.plist_installed,
        &status.plist_path,
        status.loaded,
        heartbeat.as_ref(),
        now,
    );

    if flags.contains(&"--json") {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::print_daemon_status_report(&report);
    }
}

#[cfg(target_os = "macos")]
fn daemon_run() {
    use monitor::{FilePersistence, PollConfig, SystemClock, ThresholdConfig};
    use platform::macos::{MacosFsStat, MacosNotifier};

    let history_path = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Glomeris/history.tsv"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/glomeris-history.tsv"));
    // Same `Library/Application Support/Glomeris/` directory as
    // `history_path` above — see `monitor::run`'s doc comment (HORO-1044);
    // `daemon_status`'s `heartbeat_path()` reads this back for `daemon
    // status --json` (HORO-1045).
    let heartbeat_path = heartbeat_path();

    let config = PollConfig::new("/");
    let thresholds = ThresholdConfig::default();
    let clock = SystemClock;
    let fs_stat = MacosFsStat;
    let notifier = MacosNotifier;
    let persistence = FilePersistence::new(history_path);

    // HORO-1508. Where the loop stops being only about the four built-in
    // pressure states and starts being about the user's own threshold.
    //
    // Deliberately *outside* `monitor::run`. The loop's four states are
    // Glomeris's, fixed and not configurable; an episode is the user's, and it
    // exists to decide when they are spoken to. Threading a settings file and
    // an episode store through `poll_once` would put a preference inside the
    // thing that classifies disk pressure, and a pressure state must mean the
    // same thing on every machine. The per-iteration hook is the right seam:
    // the loop reports what it measured, and this decides what to say about it.
    let episode_state_path = monitor::default_episode_state_path().ok();
    if episode_state_path.is_none() {
        // Said once, at startup, rather than every poll. The monitor still runs
        // — the four pressure states and the history it records do not depend
        // on this — so what is lost is the threshold notification and nothing
        // else, and that is worth exactly one line.
        eprintln!(
            "glomeris monitor: HOME is not set, so pressure episodes cannot be recorded and \
             threshold notifications will not be raised; disk-pressure monitoring continues"
        );
    }
    // The last settings that loaded cleanly. Kept so that saving a malformed
    // settings file cannot stop the daemon noticing a full disk: a parse error
    // means the *new* preference is unusable, not that the old one stopped
    // being what the user asked for.
    let mut settings = glomeris::settings::RecoverySettings::default();
    let mut reported_settings_error = false;

    monitor::run(
        &config,
        &thresholds,
        &clock,
        &fs_stat,
        &notifier,
        &persistence,
        &heartbeat_path,
        None,
        |outcome| {
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(e) => {
                    eprintln!("glomeris monitor: poll failed: {e}");
                    return;
                }
            };
            let Some(state_path) = episode_state_path.as_deref() else {
                return;
            };

            // Re-read every poll, not once at startup, so a threshold the user
            // changes while the daemon is running takes effect at the next poll
            // instead of at the next reboot. It is one small file read per
            // interval.
            match glomeris::settings::load_settings() {
                Ok(loaded) => {
                    settings = loaded;
                    reported_settings_error = false;
                }
                Err(e) => {
                    if !reported_settings_error {
                        eprintln!(
                            "glomeris monitor: could not read your settings ({e}); continuing \
                             with the last good alert threshold of {}",
                            settings.describe_notify_at()
                        );
                        reported_settings_error = true;
                    }
                }
            }

            episode_step(state_path, &settings, &outcome);
        },
    );
}

/// One poll's worth of episode bookkeeping (HORO-1508).
///
/// Re-read from disk every poll rather than held in memory across the loop, and
/// that is the whole reason this is a file at all: between two polls the
/// menu-bar app may have written the user's answer through `glomeris pressure
/// respond`. A daemon holding its own copy would overwrite that answer on the
/// next save and re-raise a notification the user had already dismissed —
/// which is precisely the storm this ticket exists to prevent.
///
/// The threshold is applied from `settings` at load, so an episode opened under
/// a threshold the user has since raised is judged against the new one from
/// here on.
///
/// Every failure is reported and survived. Nothing in this function can widen
/// what may be deleted — an episode decides when the user is spoken to, never
/// what may be removed — so there is no fail-closed obligation here, and
/// stopping the monitor over an unwritable notification record would trade a
/// missed banner for a blind machine.
#[cfg(target_os = "macos")]
fn episode_step(
    state_path: &std::path::Path,
    settings: &glomeris::settings::RecoverySettings,
    outcome: &monitor::PollOutcome,
) {
    let config = monitor::EpisodeConfig::new(settings.notify_at_used_percent());
    let mut tracker = monitor::load_tracker_at(state_path, config);
    let episode = tracker.observe(
        outcome.used_percent,
        outcome.free_bytes,
        monitor::persistence::unix_now_secs(),
    );

    // Logged to launchd's stderr, because an episode's life is otherwise
    // invisible: a user asking "why was I not told?" or "why was I told twice?"
    // needs these four moments and the numbers behind them.
    if let Some(id) = episode.opened {
        eprintln!(
            "glomeris monitor: pressure episode #{id} opened at {:.1}% used (your alert \
             threshold is {})",
            outcome.used_percent,
            settings.describe_notify_at()
        );
    }
    if let Some(id) = episode.closed {
        eprintln!(
            "glomeris monitor: pressure episode #{id} closed at {:.1}% used",
            outcome.used_percent
        );
    }
    if episode.notification_became_due {
        eprintln!("glomeris monitor: a pressure notification is owed and awaits the app");
    }
    if episode.snooze_elapsed {
        eprintln!("glomeris monitor: a snoozed pressure notification is owed again");
    }

    if let Err(e) = monitor::save_tracker_at(state_path, &tracker) {
        eprintln!("glomeris monitor: could not record the pressure episode: {e}");
    }
}

/// The mount the recovery loop and its preview both measure. One constant so
/// the pre-flight cannot be computed against a different volume than the run
/// that follows it.
#[cfg(target_os = "macos")]
const RECOVERY_TARGET_MOUNT: &str = "/";

/// `glomeris free --dry-run` — the pre-flight for a goal (HORO-1506).
///
/// Takes no execution lock and mutates nothing: it is a read of the volume
/// plus one discovery pass, which is exactly what the GUI needs to show
/// current usage, the goal, and the estimated opportunity *before* a user
/// commits to a destructive run. `--dry-run` on the raw `--target` form is
/// rendered on the used axis too, via
/// [`RecoveryGoal::from_free_target`](glomeris::executor::goal::RecoveryGoal::from_free_target),
/// so no surface has to display a bare percentage whose axis is unstated.
#[cfg(target_os = "macos")]
fn free_preview(
    goal: Option<glomeris::executor::goal::RecoveryGoal>,
    target: glomeris::executor::recovery_loop::FreeTarget,
    project_roots: Vec<PathBuf>,
    json: bool,
    progress_json: bool,
) {
    use glomeris::executor::goal::RecoveryGoal;
    use glomeris::monitor::{FsStat, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;

    let usage = match MacosFsStat.stat(std::path::Path::new(RECOVERY_TARGET_MOUNT)) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("glomeris free: could not read disk usage for {RECOVERY_TARGET_MOUNT}: {e}");
            std::process::exit(1);
        }
    };

    let goal = goal.unwrap_or_else(|| RecoveryGoal::from_free_target(&target, usage.total_bytes));

    let pass = discover_pass_now(project_roots, progress_json);
    let actions = glomeris::actions::ActionRegistry::builtin();
    let detect = glomeris::cli::build_detect_report(
        &pass.candidates,
        &pass.detectors,
        &actions,
        impact_context(),
        // Same grouping on the recovery pre-flight (HORO-1511): a person
        // deciding whether to start a run is exactly who needs to see that
        // nine worktrees of one project account for the bulk, and which of
        // them still hold work.
        &glomeris::cli::group_workspaces_now(&pass.candidates),
    );

    let report = glomeris::cli::recovery::build_recovery_preview_report(
        &goal,
        &usage,
        &ThresholdConfig::default(),
        detect,
    );

    if json {
        print_json_or_exit(&report);
    } else {
        glomeris::cli::recovery::print_recovery_preview_report(&report);
    }
}

#[cfg(target_os = "macos")]
fn free_run(
    goal: Option<glomeris::executor::goal::RecoveryGoal>,
    target: glomeris::executor::recovery_loop::FreeTarget,
    project_roots: Vec<PathBuf>,
    json: bool,
    progress_json: bool,
    stop_file: Option<PathBuf>,
    authority: RunAuthority,
) {
    use glomeris::actions::ActionRegistry;
    use glomeris::cli::recovery::NdjsonProgressObserver;
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::executor::recovery_loop::{
        run as run_recovery_loop, NeverStops, RecoveryConfig, RecoveryObserver, RecoveryRunRequest,
        SilentObserver, StopFile, StopSignal, SystemWallClock,
    };
    use glomeris::monitor::{FsStat, SystemClock, ThresholdConfig};
    use glomeris::platform::macos::MacosFsStat;
    use glomeris::policy::PolicyConfig;
    use std::time::Duration;

    let fs_stat = MacosFsStat;

    // A used-percent goal is validated against the real volume before the
    // lock is taken or anything is deleted (HORO-1506 AC4). A goal at or
    // above current usage would delete nothing and still print a report
    // that reads like a successful cleanup; refusing it here, with both
    // numbers in the message, is the difference between a clear correction
    // and a user believing Glomeris tidied up. The raw `--target` floor is
    // deliberately not subject to this — see `run_free_command`'s doc.
    if let Some(goal) = goal {
        match fs_stat.stat(std::path::Path::new(RECOVERY_TARGET_MOUNT)) {
            Ok(usage) => {
                if let Err(rejection) = goal.progress_toward(&usage) {
                    if json {
                        let report =
                            glomeris::cli::recovery::build_goal_rejection_report(&rejection);
                        print_json_or_exit(&report);
                    } else {
                        eprintln!("glomeris free: {rejection}");
                    }
                    std::process::exit(2);
                }
            }
            Err(e) => {
                eprintln!(
                    "glomeris free: could not read disk usage for {RECOVERY_TARGET_MOUNT}: {e}"
                );
                std::process::exit(1);
            }
        }
    }

    // Read, and refused if it is not a grant, BEFORE the execution lock is
    // taken: an unauthorized run has nothing to protect from a concurrent one,
    // and blocking a real `free` in another terminal in order to print a
    // refusal would be the wrong trade. Exit 3 matches `glomeris autopilot
    // run`, so a caller distinguishes "not authorized" from a usage error (2)
    // and from a failed run (1) without reading prose (HORO-1510).
    let envelope = if authority.uses_envelope() {
        let envelope = match glomeris::autopilot::load_envelope() {
            Ok(envelope) => envelope,
            Err(e) => autopilot_store_error_exit("read", e),
        };
        if !envelope.is_enabled() {
            eprintln!(
                "glomeris free: --autopilot runs only inside a grant you wrote, and Autopilot \
                 is not enabled. Nothing was attempted."
            );
            eprintln!("Enable a narrow envelope first, e.g.");
            eprintln!("  glomeris autopilot enable --kinds node_modules --max-actions 1");
            std::process::exit(3);
        }
        // A grant that authorizes a run somebody asks for is not a grant to
        // start one unasked, and this is where that distinction is enforced
        // rather than trusted to the caller. Same exit code as above, because
        // to a script both are the same fact: this run was not authorized.
        if authority.is_unprompted() && !envelope.starts_unprompted() {
            eprintln!(
                "glomeris free: --unattended needs a grant that allows starting a run without \
                 being asked, and this one does not. Nothing was attempted."
            );
            eprintln!("Add that permission explicitly, e.g.");
            eprintln!(
                "  glomeris autopilot enable --kinds node_modules --max-actions 1 \
                 --respond-to-alerts"
            );
            std::process::exit(3);
        }
        // Printed before anything runs, for the same reason `autopilot run`
        // prints it: the output of a run that deletes should open with the
        // authority it acted under rather than asking the reader to go and look
        // it up afterwards. To stderr, not stdout, because a `--json` caller's
        // stdout carries exactly one report.
        eprintln!("Autopilot envelope:");
        for line in envelope.describe() {
            eprintln!("  {line}");
        }
        eprintln!();
        Some(envelope)
    } else {
        None
    };

    // Unobserved is not "fine": the gate fails a configured pressure floor
    // closed against a missing observation. Read once, here, because a run that
    // is working lowers the very pressure that admitted it.
    let observed_pressure = envelope.as_ref().map(|_| {
        match fs_stat.stat(std::path::Path::new(RECOVERY_TARGET_MOUNT)) {
            Ok(usage) => {
                Some(ThresholdConfig::default().classify(usage.used_percent(), usage.free_bytes))
            }
            Err(e) => {
                eprintln!(
                    "glomeris free: could not read disk usage ({e}) — pressure is unobserved"
                );
                None
            }
        }
    });

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns. `json` is threaded through so a busy lock answers a
    // machine-readable caller with a `"busy"` refusal rather than an empty
    // stdout and a bare exit code.
    let _execution_lock = acquire_execution_lock_or_exit("free", json);

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let discovery_ctx = DiscoveryContext::new(home_dir).with_known_project_roots(project_roots);

    let config = RecoveryConfig {
        target,
        max_iterations: 100,
        // Under an envelope these come down to the envelope's own figures, so
        // that the gate is always the stricter of the two and a stop is
        // attributed to the limit the user actually wrote. Left as they were,
        // the loop's 600-second ceiling could cut short a run the user had
        // authorized for the full 15 minutes and report `budget_exceeded` — a
        // true sentence about the wrong budget (HORO-1510).
        max_actions: envelope
            .as_ref()
            .map_or(100, glomeris::autopilot::AutopilotEnvelope::max_actions),
        max_duration: envelope.as_ref().map_or_else(
            || Duration::from_secs(600),
            glomeris::autopilot::AutopilotEnvelope::max_duration,
        ),
        // No interactive prompt in this MVP — Ask candidates are
        // reported as declined/skipped rather than executed. See
        // RecoveryConfig::auto_approve_ask's doc comment.
        //
        // An envelope does not change this. What it can do is pre-authorize a
        // *specific* Ask risk, recorded in the grant, which the gate honours
        // per candidate — a blanket yes and a written-down yes are not the same
        // consent.
        auto_approve_ask: false,
    };

    let collector = DefaultEvidenceCollector::default();
    let detector_registry = DetectorRegistry::builtin();
    let action_registry = ActionRegistry::builtin();
    let clock = SystemClock;
    let wall_clock = SystemWallClock;
    let policy_cfg = PolicyConfig::default();
    // Progress goes to stderr, never stdout: a `--json` caller's stdout carries
    // exactly one report, and a stream mixed into it would stop being parseable
    // as one document. Without the flag the run is observed by nobody, and
    // stdout and stderr stay byte-identical to a pre-HORO-1509 run.
    let observer: Box<dyn RecoveryObserver> = if progress_json {
        Box::new(NdjsonProgressObserver::new(std::io::stderr()))
    } else {
        Box::new(SilentObserver)
    };
    // A sentinel path the caller creates when the user presses "Stop after
    // current action". `StopFile::watching` refuses a path that already exists,
    // and that refusal happens here — before the lock is used to delete
    // anything — because a stale sentinel from an earlier run would stop this
    // one immediately and the report would truthfully say the user stopped it
    // while the user had done nothing at all.
    let stop_signal: Box<dyn StopSignal> = match &stop_file {
        Some(path) => match StopFile::watching(path) {
            Ok(watcher) => Box::new(watcher),
            Err(e) => {
                eprintln!(
                    "glomeris free: cannot watch stop file {}: {e}",
                    path.display()
                );
                std::process::exit(2);
            }
        },
        None => Box::new(NeverStops),
    };

    let audit_log_path = actions_jsonl_path();
    let report = run_recovery_loop(RecoveryRunRequest {
        config: &config,
        fs_stat: &fs_stat,
        collector: &collector,
        detector_registry: &detector_registry,
        action_registry: &action_registry,
        clock: &clock,
        wall_clock: &wall_clock,
        policy_cfg: &policy_cfg,
        target_mount: std::path::Path::new(RECOVERY_TARGET_MOUNT),
        discovery_ctx: &discovery_ctx,
        audit_log_path: &audit_log_path,
        observer: observer.as_ref(),
        stop: stop_signal.as_ref(),
        // Absent unless `--autopilot` was passed, in which case a human typed
        // this command and the only limits are the ones above. Present, it is a
        // second gate and never a second policy: it can only withhold
        // candidates the loop would otherwise have taken (HORO-1510).
        admission: envelope.as_ref().map(|envelope| {
            glomeris::executor::recovery_loop::RecoveryAdmission {
                envelope,
                observed_pressure: observed_pressure.flatten(),
            }
        }),
    });

    if json {
        // `total_bytes` is re-read rather than remembered from the
        // validation above: `target_met` must be decided against the volume
        // as it is now, and a capacity that changed under us (an unmounted
        // or resized volume) should show up as a read failure rather than be
        // silently paired with fresh free-space figures. A failure here does
        // not undo the run, so it reports and exits non-zero rather than
        // claiming a total it does not have.
        let total_bytes = match fs_stat.stat(std::path::Path::new(RECOVERY_TARGET_MOUNT)) {
            Ok(usage) => usage.total_bytes,
            Err(e) => {
                eprintln!(
                    "glomeris free: run finished but disk capacity could not be re-read \
                     for {RECOVERY_TARGET_MOUNT}: {e}"
                );
                std::process::exit(1);
            }
        };
        let run_report = glomeris::cli::recovery::build_recovery_run_report(
            goal.as_ref(),
            &config.target,
            total_bytes,
            &report,
        );
        print_json_or_exit(&run_report);
    } else {
        print_recovery_report(goal.as_ref(), &config.target, &report);
    }
}

#[cfg(not(target_os = "macos"))]
fn free_preview(
    _goal: Option<glomeris::executor::goal::RecoveryGoal>,
    _target: glomeris::executor::recovery_loop::FreeTarget,
    _project_roots: Vec<PathBuf>,
    _json: bool,
    _progress_json: bool,
) {
    eprintln!("glomeris free: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn free_run(
    _goal: Option<glomeris::executor::goal::RecoveryGoal>,
    _target: glomeris::executor::recovery_loop::FreeTarget,
    _project_roots: Vec<PathBuf>,
    _json: bool,
    _progress_json: bool,
    _stop_file: Option<PathBuf>,
    _authority: RunAuthority,
) {
    eprintln!("glomeris free: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_install(_force: bool) {
    eprintln!("glomeris daemon install: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_uninstall() {
    eprintln!("glomeris daemon uninstall: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_status(_args: &[String]) {
    eprintln!("glomeris daemon status: only supported on macOS");
    std::process::exit(1);
}

#[cfg(not(target_os = "macos"))]
fn daemon_run() {
    eprintln!("glomeris daemon run: only supported on macOS");
    std::process::exit(1);
}
