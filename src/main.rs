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
/// `json`: HORO-1056 gap fix. `emergency`/`free` have no `--json` mode at
/// all, so they always pass `false` here and this behaves exactly as
/// before. `execute` DOES have `--json` (HORO-1055) and parses it before
/// ever reaching this call site (see `run_execute_command`) — passing it
/// through here means a `--json` caller gets a structured `"busy"` report
/// on stdout instead of silence plus a bare exit code, matching every
/// other refusal path `render_execute_resolution` already renders. This
/// is the one refusal/abort path in `execute` that happens BEFORE
/// `glomeris::cli::ExecuteResolution` exists at all, which is why it is
/// not one of that enum's variants and is rendered here instead.
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

fn run_free_command(args: &[String]) {
    let (project_roots, remaining) = match glomeris::cli::extract_project_roots(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("glomeris free: {e}");
            print_command_usage("free");
            std::process::exit(2);
        }
    };

    let mut target_arg: Option<&str> = None;
    let mut i = 0;
    while i < remaining.len() {
        if remaining[i] == "--target" {
            target_arg = remaining.get(i + 1).map(String::as_str);
            i += 2;
        } else {
            eprintln!("glomeris free: unrecognized argument '{}'", remaining[i]);
            print_command_usage("free");
            std::process::exit(2);
        }
    }

    let target_arg = match target_arg {
        Some(t) => t,
        None => {
            eprintln!("glomeris free: --target is required");
            print_command_usage("free");
            std::process::exit(2);
        }
    };

    let target = match glomeris::executor::recovery_loop::parse_free_target(target_arg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("glomeris free: {e}");
            std::process::exit(2);
        }
    };

    free_run(target, project_roots);
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

/// Splits `--json` off an autopilot argument list, returning whether it was
/// present and whatever else was there.
///
/// Separate from [`split_flags`] because the autopilot verbs reject anything
/// they do not recognize rather than collecting it, so the remainder has to
/// stay `String` for the existing rejection paths to keep reporting the
/// offending token.
fn autopilot_take_json_flag(args: &[String]) -> (bool, Vec<String>) {
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
    let (json, rest) = autopilot_take_json_flag(args);
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
    let (json, rest) = autopilot_take_json_flag(args);
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

fn print_recovery_report(report: &glomeris::executor::recovery_loop::RecoveryReport) {
    use glomeris::reporting::human_bytes;

    println!("stop reason:            {:?}", report.stop_reason);
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
    // was concluded without having looked everywhere (HORO-1484).
    if !report.detector_failures.is_empty() {
        println!(
            "discovery incomplete:   {} detector(s) failed",
            report.detector_failures.len()
        );
        for failure in &report.detector_failures {
            println!("  - {failure}");
        }
        if report.stop_reason == glomeris::executor::recovery_loop::StopReason::SafeExhausted {
            println!(
                "note: this run stopped because no safe candidate remained among the \
                 detectors that answered; it is not a finding that nothing safe is left"
            );
        }
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
            if let Err(e) = outcome {
                eprintln!("glomeris monitor: poll failed: {e}");
            }
        },
    );
}

#[cfg(target_os = "macos")]
fn free_run(target: glomeris::executor::recovery_loop::FreeTarget, project_roots: Vec<PathBuf>) {
    use glomeris::actions::ActionRegistry;
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::DefaultEvidenceCollector;
    use glomeris::executor::recovery_loop::{
        run as run_recovery_loop, RecoveryConfig, SystemWallClock,
    };
    use glomeris::monitor::SystemClock;
    use glomeris::platform::macos::MacosFsStat;
    use glomeris::policy::PolicyConfig;
    use std::time::Duration;

    // HORO-1054: held for the duration of the real-execution portion
    // below, released automatically (via `Drop`) when this function
    // returns.
    let _execution_lock = acquire_execution_lock_or_exit("free", false);

    let home_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let discovery_ctx = DiscoveryContext::new(home_dir).with_known_project_roots(project_roots);

    let config = RecoveryConfig {
        target,
        max_iterations: 100,
        max_actions: 100,
        max_duration: Duration::from_secs(600),
        // No interactive prompt in this MVP — Ask candidates are
        // reported as declined/skipped rather than executed. See
        // RecoveryConfig::auto_approve_ask's doc comment.
        auto_approve_ask: false,
    };

    let fs_stat = MacosFsStat;
    let collector = DefaultEvidenceCollector::default();
    let detector_registry = DetectorRegistry::builtin();
    let action_registry = ActionRegistry::builtin();
    let clock = SystemClock;
    let wall_clock = SystemWallClock;
    let policy_cfg = PolicyConfig::default();

    let audit_log_path = actions_jsonl_path();
    let report = run_recovery_loop(
        &config,
        &fs_stat,
        &collector,
        &detector_registry,
        &action_registry,
        &clock,
        &wall_clock,
        &policy_cfg,
        std::path::Path::new("/"),
        &discovery_ctx,
        &audit_log_path,
    );

    print_recovery_report(&report);
}

#[cfg(not(target_os = "macos"))]
fn free_run(_target: glomeris::executor::recovery_loop::FreeTarget, _project_roots: Vec<PathBuf>) {
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
