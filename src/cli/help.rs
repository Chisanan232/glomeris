//! The CLI's help architecture (HORO-1311).
//!
//! # Why this is a module and not a set of `println!`s in `main.rs`
//!
//! HORO-1050 already established the principle: `--help` and the usage shown
//! on a bad command must come from one table, because when they were two
//! strings they drifted (HORO-1034 — `print_usage()` silently omitted
//! `llm-plan`). That table lived in `main.rs`, which meant nothing but a
//! spawned-binary integration test could see it, and the one test that
//! checked it kept its own hand-written mirror of the subcommand list. That
//! mirror had itself drifted by the time this ticket started: it was missing
//! `llm-check`, so "every subcommand supports `--help`" was being asserted
//! over twelve of the thirteen subcommands.
//!
//! So the table moves here, into the library, where tests can read
//! [`COMMANDS`] directly instead of re-typing it. Everything the CLI says
//! about itself — top-level help, per-command help, the short usage line on a
//! bad command, and the exit-code reference — is rendered from this one array
//! by the functions below. There is no second copy to keep in step.
//!
//! # The three axes this help text keeps apart
//!
//! A command's [`Safety`] is **not** a resource's policy class. `glomeris
//! detect` being read-only says nothing about whether the things it finds are
//! `AUTO_SAFE`; `glomeris execute` being able to delete says nothing about
//! whether it *will* — a `PROTECTED` resource refuses every flag combination
//! that exists. Help text that blurred those two would teach exactly the
//! wrong mental model, so [`Safety`] describes only one thing: whether
//! running this command can change your filesystem. What happens to any
//! individual resource is policy's answer, given per resource, at execution
//! time.
//!
//! # What this module is not allowed to do
//!
//! Decide anything. It holds no flag parser, no dispatch, and no defaults —
//! `main.rs` still owns all of that. Rendering help must never be able to
//! disagree with what the binary actually accepts, so where a string here
//! describes behaviour, the behaviour stays in `main.rs` and this is a
//! description of it. The golden tests in `tests/help_golden.rs` are
//! what stop the description from rotting.

/// Whether running a command can change the filesystem.
///
/// Deliberately more states than a `bool`, because the ones in the middle are
/// the product's whole thesis: `llm-plan` contacts a model and produces a
/// proposal, and a proposal is not a mutation; `daemon install` writes a
/// launch agent and deletes nothing of yours. A user who cannot tell
/// "suggests" from "does", or "sets itself up" from "deletes your files",
/// either fears the safe commands or trusts the dangerous ones.
///
/// # The variant order is load-bearing (HORO-1485)
///
/// `Ord` is derived, and the variants are declared weakest-first, so
/// `a.max(b)` is "the stronger claim of the two". A command that dispatches
/// subcommands declares the strongest safety reachable through it, and
/// `tests::command_safety_covers_every_reachable_subcommand` compares the two
/// with exactly that ordering. Reordering these variants would silently
/// change what that test asserts, which is why the order is documented here
/// rather than left to look alphabetical-by-accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    /// Reads and reports. Changes nothing, sends nothing.
    ReadOnly,
    /// Produces a proposal. Executes nothing. May contact a configured
    /// provider (`llm-plan`, `llm-check`) — which is network access, not
    /// filesystem mutation, and is called out separately where it applies.
    Advisory,
    /// Writes files Glomeris itself owns, and nothing else: the launch agent
    /// plist (`daemon install`, removed again by `daemon uninstall`), the
    /// monitor's own history and heartbeat records (`daemon run`), the
    /// Autopilot envelope (`autopilot enable`, `autopilot revoke`).
    ///
    /// Its own state, not your data — which is why it is neither `ReadOnly`
    /// nor `Destructive` (HORO-1485). Calling `daemon install` read-only was a
    /// false reassurance: it writes a plist and loads a background agent.
    /// Calling it destructive would be a false warning, and a reader who has
    /// been warned once for nothing discounts the next warning too.
    WritesOwnState,
    /// Can delete data, under policy control and never unconditionally.
    Destructive,
}

impl Safety {
    /// Every state, weakest first. Iterated by tests so that adding a variant
    /// cannot quietly leave it uncovered by the ones that check every label.
    pub const ALL: &'static [Safety] = &[
        Safety::ReadOnly,
        Safety::Advisory,
        Safety::WritesOwnState,
        Safety::Destructive,
    ];

    /// The word used in per-command help, on its own line above the
    /// description.
    pub fn label(self) -> &'static str {
        match self {
            Safety::ReadOnly => "Read-only — changes nothing.",
            Safety::Advisory => "Advisory — proposes, never executes.",
            Safety::WritesOwnState => "Writes only Glomeris's own state — never your files.",
            Safety::Destructive => "Can delete data — every deletion is policy-gated.",
        }
    }
}

/// The five groups top-level help is organized into.
///
/// Ordered as a session runs, not alphabetically: look at the machine, decide
/// what to do, do it, check what happened, and only then the background
/// service. A first-time reader should be able to start at the top and stop
/// when they have what they came for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Inspect,
    Plan,
    Act,
    Observe,
    Service,
}

impl Group {
    /// Every group, in display order.
    pub const ALL: &'static [Group] = &[
        Group::Inspect,
        Group::Plan,
        Group::Act,
        Group::Observe,
        Group::Service,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Inspect => "INSPECT",
            Group::Plan => "PLAN",
            Group::Act => "ACT",
            Group::Observe => "OBSERVE",
            Group::Service => "SERVICE",
        }
    }

    /// The one line under the group heading. This is where a reader learns
    /// the consequence of the whole group before reading any command in it,
    /// which is the point of grouping them at all.
    pub fn caption(self) -> &'static str {
        match self {
            Group::Inspect => "look at this machine. Nothing is changed.",
            Group::Plan => "decide what to do. Nothing is executed.",
            Group::Act => "change this machine. Policy decides every deletion.",
            Group::Observe => "what happened before. Nothing is changed.",
            Group::Service => "the background disk-pressure monitor.",
        }
    }
}

/// One verb a command dispatches on, with the safety of *that verb* (HORO-1485).
///
/// # Why a subcommand needs its own safety and not just a description
///
/// `glomeris daemon --help` printed "Read-only — changes nothing." above a
/// list containing `install` and `uninstall`. The description of each verb was
/// accurate — "Write and load the launch agent" — and the banner over them
/// was false, which is the worse of the two to get wrong: a reader who
/// believes the banner does not read the descriptions looking for a
/// contradiction.
///
/// The verbs used to be [`OptionSpec`] entries, which is where the false
/// banner came from: an option cannot carry a consequence, so a command whose
/// consequences differ per verb had nowhere to say so and had to pick one
/// claim for all of them. A subcommand is not an option, so it gets its own
/// type and its own [`Safety`].
pub struct SubcommandSpec {
    /// The literal verb, e.g. `"install"`. Matched against `main.rs`'s
    /// dispatch by `tests/subcommand_safety_is_honest.rs`, so a verb the
    /// binary accepts cannot go undeclared here — and therefore cannot be
    /// reachable without a stated consequence.
    pub name: &'static str,
    /// What may follow the verb, rendered after it, e.g. `"[--force]"`. Empty
    /// when the verb takes nothing.
    pub args: &'static str,
    /// What *this verb* can do. The command's own [`CommandSpec::safety`] is
    /// the strongest of these; see
    /// `tests::command_safety_covers_every_reachable_subcommand`.
    pub safety: Safety,
    pub description: &'static str,
}

impl SubcommandSpec {
    /// The verb as it appears in help: the name, plus its arguments when it
    /// has any. One function so the rendered form cannot drift from the name
    /// the dispatch grounding test matches.
    pub fn syntax(&self) -> String {
        if self.args.is_empty() {
            self.name.to_string()
        } else {
            format!("{} {}", self.name, self.args)
        }
    }
}

/// One named option and what it does, for a command's `Options` block.
pub struct OptionSpec {
    /// Rendered verbatim in the left column, e.g. `"--project-root <path>"`.
    pub syntax: &'static str,
    pub description: &'static str,
}

/// One example invocation and the question it answers.
///
/// Paired deliberately: a bare list of commands shows syntax, while a command
/// beside the question it answers shows *when to reach for it*, which is what
/// a first-time user is actually missing.
pub struct ExampleSpec {
    pub command: &'static str,
    pub purpose: &'static str,
}

/// One exit status and what it means for this command.
pub struct ExitCodeSpec {
    pub code: i32,
    pub meaning: &'static str,
}

/// Everything the CLI knows how to say about one subcommand.
pub struct CommandSpec {
    /// The literal first positional argument, e.g. `"llm-plan"`.
    pub name: &'static str,
    pub group: Group,
    /// The strongest thing typing this command's name can lead to.
    ///
    /// For a command that dispatches verbs, that is the strongest safety in
    /// [`CommandSpec::subcommands`] — asserted, not assumed, by
    /// `tests::command_safety_covers_every_reachable_subcommand` (HORO-1485).
    /// A command must never under-state here: this is the claim a reader sees
    /// before they know which verb they will type.
    pub safety: Safety,
    /// One line for the top-level list. Kept short enough to sit in a column
    /// beside the name without wrapping on an 80-column terminal.
    pub summary: &'static str,
    /// The usage fragment, without the leading `glomeris `, split into the
    /// units that must not be broken across lines.
    ///
    /// Segments rather than one string because some of these are long — the
    /// full `execute` invocation is 160 columns — and the one thing worse than
    /// a line that wraps in the terminal is a line this code wrapped in the
    /// wrong place. `[--confirm-ask --observed-fingerprint <token>]` is a
    /// single segment for exactly that reason: the two flags are only valid
    /// together, and a line break between them would suggest otherwise.
    /// Element 0 carries the command name and any required positional.
    pub usage: &'static [&'static str],
    /// The paragraph shown under the usage line in per-command help. This is
    /// where a command says what it will and will not do — the sentences AC 3,
    /// 4 and 5 are about.
    pub details: &'static str,
    /// The verbs this command dispatches on, each with its own consequence.
    /// Empty for a command that takes only flags.
    pub subcommands: &'static [SubcommandSpec],
    pub options: &'static [OptionSpec],
    pub examples: &'static [ExampleSpec],
    /// This command's own exit codes. Only the ones that differ from, or
    /// matter more than, the shared table in [`render_exit_codes`] — a
    /// per-command list that restated all of them would be the wall of text
    /// this ticket exists to remove.
    pub exit_codes: &'static [ExitCodeSpec],
    /// Sibling commands worth knowing about, by name. Verified against
    /// [`COMMANDS`] by test, so a renamed command cannot leave a dangling
    /// pointer here.
    pub see_also: &'static [&'static str],
}

/// Every registered top-level subcommand.
///
/// Adding a subcommand to `main.rs`'s dispatch without adding a row here is
/// caught by `tests/shared_command_table.rs`, which asserts the two agree in both
/// directions by spawning the real binary.
pub const COMMANDS: &[CommandSpec] = &[
    // ----------------------------------------------------------------- Inspect
    CommandSpec {
        name: "status",
        group: Group::Inspect,
        safety: Safety::ReadOnly,
        summary: "Current disk-pressure state and the thresholds behind it.",
        usage: &["status", "[--json]"],
        details: "Reads the filesystem's free space and reports which pressure state that \
                  falls into, together with the thresholds used to decide. Runs no detectors, \
                  so it is fast and says nothing about individual resources.",
        subcommands: &[],
        options: &[OptionSpec {
            syntax: "--json",
            description: "Print a StatusReport as JSON on stdout instead of prose.",
        }],
        examples: &[ExampleSpec {
            command: "glomeris status",
            purpose: "Is anything wrong right now?",
        }],
        exit_codes: &[],
        see_also: &["detect", "history"],
    },
    CommandSpec {
        name: "scan",
        group: Group::Inspect,
        safety: Safety::ReadOnly,
        summary: "Largest directories under one path, by size.",
        usage: &["scan", "[path]", "[top_k]"],
        details: "Walks one directory tree and reports its largest entries. This is the raw \
                  size picture: no evidence correlation, no policy classification, and no \
                  notion of whether anything found is reclaimable — for that, use `detect`. \
                  Note the default path is the current directory, not your whole machine.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "[path]",
                description: "Where to start walking. Defaults to the current directory.",
            },
            OptionSpec {
                syntax: "[top_k]",
                description: "How many of the largest entries to print. Defaults to 20. \
                              Positional, so it must follow a path — a lone number is read as \
                              a path and finds nothing.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris scan ~/Library/Developer 30",
                purpose: "What is biggest under one directory?",
            },
            ExampleSpec {
                command: "glomeris scan",
                purpose: "Same, for the directory you are standing in.",
            },
        ],
        exit_codes: &[],
        see_also: &["detect", "status"],
    },
    CommandSpec {
        name: "detect",
        group: Group::Inspect,
        safety: Safety::ReadOnly,
        summary: "Find reclaimable candidates and how each is classified.",
        usage: &[
            "detect",
            "[--project-root <path>]...",
            "[--json]",
            "[--progress-json]",
        ],
        details: "Runs every detector, correlates the evidence each one produces, and prints \
                  the candidates with their policy classification and size estimate. Deletes \
                  nothing and sends nothing anywhere. A candidate's size is not a safety \
                  judgement: a large AUTO_SAFE cache is an opportunity, and a small PROTECTED \
                  resource is still protected.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Scope the cargo and node detectors to this project. Repeatable. \
                              Does not bound the Xcode detector (which is $HOME-bounded) or \
                              the Homebrew and Docker detectors (which are bounded by neither).",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print a DetectReport as JSON on stdout.",
            },
            OptionSpec {
                syntax: "--progress-json",
                description: "Stream one JSON progress event per line to stderr while \
                              detectors run. stdout stays a single clean document.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris detect",
                purpose: "What could I reclaim?",
            },
            ExampleSpec {
                command: "glomeris detect --project-root ~/code/my-app --json",
                purpose: "Machine-readable, scoped to one project.",
            },
        ],
        exit_codes: &[],
        see_also: &["explain", "clean", "free"],
    },
    CommandSpec {
        name: "explain",
        group: Group::Inspect,
        safety: Safety::ReadOnly,
        summary: "Why one resource is classified the way it is.",
        usage: &[
            "explain <resource_id_or_path>",
            "[--project-root <path>]...",
            "[--json]",
            "[--progress-json]",
        ],
        details: "Shows the full evidence-and-policy picture for a single resource: what was \
                  observed, which rule that triggered, and what would be offered for it. This \
                  is the command to reach for when a classification looks wrong — it shows \
                  the reasoning, not just the verdict. With --json it also emits the \
                  fingerprint_token that `execute` requires for an ASK-classified resource.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "<resource_id_or_path>",
                description: "Required. Either a resource id as printed by `detect`, or a \
                              filesystem path.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable.",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print an ExplainReport as JSON on stdout, including \
                              fingerprint_token.",
            },
            OptionSpec {
                syntax: "--progress-json",
                description: "Stream progress events as JSON lines to stderr.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris explain ~/Library/Developer/Xcode/DerivedData",
                purpose: "Why is that safe, or not?",
            },
            ExampleSpec {
                command: "glomeris explain cargo_target_dir:/Users/me/proj/target --json",
                purpose: "Capture fingerprint_token before an ASK execution.",
            },
        ],
        exit_codes: &[],
        see_also: &["detect", "execute"],
    },
    // -------------------------------------------------------------------- Plan
    CommandSpec {
        name: "clean",
        group: Group::Plan,
        safety: Safety::Advisory,
        summary: "Render what would be cleaned. Executes nothing.",
        usage: &[
            "clean --dry-run",
            "[--target <resource_id_or_path>]",
            "[--project-root <path>]...",
        ],
        details: "Builds the real plan for every candidate (or one, with --target) and prints \
                  it without running it. --dry-run is required and there is no flag that \
                  removes it: this command has no executing mode to fall into by typo. To \
                  actually act, use `execute` for one resource or `free` for a target.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--dry-run",
                description: "Required. Omitting it is a usage error, not an execution.",
            },
            OptionSpec {
                syntax: "--target <resource_id_or_path>",
                description: "Restrict the output to one resource. Without it, every \
                              discovered candidate is rendered.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable.",
            },
        ],
        examples: &[ExampleSpec {
            command: "glomeris clean --dry-run",
            purpose: "What would be removed, exactly?",
        }],
        exit_codes: &[],
        see_also: &["detect", "execute", "free"],
    },
    CommandSpec {
        name: "llm-plan",
        group: Group::Plan,
        safety: Safety::Advisory,
        summary: "Ask a configured LLM for a suggestion. Advisory only.",
        // The old one-line form was `llm-plan <--schema|--print-payload|...>`,
        // whose angle brackets said one of these is required. None is: a bare
        // `llm-plan` is the live run. The modes are explained below instead of
        // being encoded in punctuation that was saying the wrong thing.
        usage: &[
            "llm-plan",
            "[--schema]",
            "[--print-payload]",
            "[--plan-file <path>]",
            "[--project-root <path>]...",
            "[--json]",
            "[--progress-json]",
        ],
        details: "Produces a suggestion and stops. Nothing a model returns is executed, and \
                  nothing it returns can invent an action: a plan can only select action ids \
                  this binary already registered, which are then re-checked against evidence \
                  this process collected itself. A model cannot override policy, supply a \
                  path, or run a command. Requires a BYOK provider you configure; no \
                  provider is built into Glomeris.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--schema",
                description: "Print an example LlmPlan document and exit. Contacts nothing.",
            },
            OptionSpec {
                syntax: "--print-payload",
                description: "Print the exact request a live run would send, without sending \
                              it and without reading a credential. Shows which fields leave \
                              this Mac and which stay.",
            },
            OptionSpec {
                syntax: "--plan-file <path>",
                description: "Read a plan from a file instead of calling a provider. Offline, \
                              and the way to replay a captured plan.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable. Note it does not \
                              bound every detector, so it is not a privacy control.",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print an LlmPlanReport as JSON on stdout.",
            },
            OptionSpec {
                syntax: "--progress-json",
                description: "Stream progress events as JSON lines to stderr.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris llm-plan --print-payload",
                purpose: "What would be sent, before I send anything?",
            },
            ExampleSpec {
                command: "glomeris llm-plan --json",
                purpose: "Get a suggestion for this machine.",
            },
        ],
        exit_codes: &[
            ExitCodeSpec {
                code: 1,
                meaning: "the provider call or response parsing failed, or --plan-file named \
                          an unreadable path.",
            },
            ExitCodeSpec {
                code: 2,
                meaning: "an unrecognized argument, a missing flag value, a credential passed \
                          as a flag, or no provider configured for a live run.",
            },
        ],
        see_also: &["llm-check", "clean", "execute"],
    },
    CommandSpec {
        name: "llm-check",
        group: Group::Plan,
        safety: Safety::Advisory,
        summary: "Test the configured LLM endpoint, credential and model.",
        usage: &["llm-check", "[--json]"],
        details: "Sends one request containing two fixed words and reports what came back. It \
                  runs no detectors, reads no project roots, collects no evidence and \
                  consults no policy, so it describes nothing about this machine to the \
                  provider. It never prints your credential. Use it to tell a wrong address \
                  apart from a wrong key apart from a wrong model name.",
        subcommands: &[],
        options: &[OptionSpec {
            syntax: "--json",
            description: "Print an LlmCheckReport as JSON on stdout.",
        }],
        examples: &[ExampleSpec {
            command: "glomeris llm-check",
            purpose: "Does my provider setup actually work?",
        }],
        exit_codes: &[
            ExitCodeSpec {
                code: 0,
                meaning: "the provider answered and the reply was usable.",
            },
            ExitCodeSpec {
                code: 1,
                meaning: "the check ran and did not pass. The report is printed first — it \
                          names which of the endpoint, credential or model to look at.",
            },
            ExitCodeSpec {
                code: 2,
                meaning: "nothing was sent and there is no report: no provider configured, a \
                          base URL that cannot work, or an unrecognized argument.",
            },
        ],
        see_also: &["llm-plan"],
    },
    // --------------------------------------------------------------------- Act
    CommandSpec {
        name: "execute",
        group: Group::Act,
        safety: Safety::Destructive,
        summary: "Run one action against one resource, under policy.",
        usage: &[
            "execute --action-id <id> --resource-id <id>",
            // One segment, not two: these are only ever valid together, and
            // splitting them across lines would invite passing just one.
            "[--confirm-ask --observed-fingerprint <token>]",
            "[--project-root <path>]...",
            "[--json]",
            "[--progress-json]",
        ],
        details: "The only command that performs one specific deletion you named. You supply \
                  selectors and nothing else: there is no flag to pass a policy class, a raw \
                  path as a target, a shell string, or a --force override. \
                  \n\nWhat happens to the resource is policy's decision, not yours and not \
                  this flag list's. AUTO_SAFE proceeds with no confirmation. ASK requires \
                  --confirm-ask together with the exact --observed-fingerprint token from a \
                  prior `explain --json` on that same resource — a fingerprint this process \
                  observed itself would defeat the point, so it is not accepted. PROTECTED \
                  refuses unconditionally: no combination of flags that exists can authorize \
                  it. Even after authorization, a revalidation immediately before deletion \
                  aborts the plan if the resource changed in between.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--action-id <id>",
                description: "Required. Must match the action the resolved resource actually \
                              offers, as printed by `actions list` or `explain`.",
            },
            OptionSpec {
                syntax: "--resource-id <id>",
                description: "Required. A resource id as printed by `detect` or `explain`.",
            },
            OptionSpec {
                syntax: "--confirm-ask",
                description: "Supply consent for an ASK-classified resource. Must be passed \
                              together with --observed-fingerprint or not at all.",
            },
            OptionSpec {
                syntax: "--observed-fingerprint <token>",
                description: "The fingerprint_token from a prior `explain --json` on this \
                              resource, copied verbatim. Never hand-constructed.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable.",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print an ExecuteReport, or an ExecuteRefusalReport naming which \
                              refusal fired, on stdout. Never silent on a non-executed \
                              outcome.",
            },
            OptionSpec {
                syntax: "--progress-json",
                description: "Stream progress events as JSON lines to stderr.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris execute --action-id cargo.clean.target_dir \\\n    \
                          --resource-id cargo_target_dir:/Users/me/proj/target --json",
                purpose: "An AUTO_SAFE resource needs no confirmation flags.",
            },
            ExampleSpec {
                command: "glomeris execute --action-id <id> --resource-id <id> \\\n    \
                          --confirm-ask --observed-fingerprint \"$TOKEN\"",
                purpose: "An ASK resource, with consent pinned to what `explain` saw.",
            },
        ],
        exit_codes: &[
            ExitCodeSpec {
                code: 0,
                meaning: "the action executed and succeeded.",
            },
            ExitCodeSpec {
                code: 1,
                meaning: "the action executed but a step of the plan failed.",
            },
            ExitCodeSpec {
                code: 2,
                meaning: "usage error, including --confirm-ask without a fingerprint or a \
                          malformed token.",
            },
            ExitCodeSpec {
                code: 3,
                meaning: "refused by policy: PROTECTED, ASK with no consent, or a consent \
                          that did not match.",
            },
            ExitCodeSpec {
                code: 4,
                meaning: "aborted by revalidation — the resource changed between \
                          authorization and deletion.",
            },
            ExitCodeSpec {
                code: 5,
                meaning: "the resource id matched nothing, offered no action, or offered a \
                          different one.",
            },
            ExitCodeSpec {
                code: 75,
                meaning: "another glomeris invocation holds the execution lock.",
            },
        ],
        see_also: &["explain", "clean", "actions"],
    },
    CommandSpec {
        name: "free",
        group: Group::Act,
        safety: Safety::Destructive,
        summary: "Reclaim space until a target is reached.",
        usage: &["free --target <N%|NB>", "[--project-root <path>]..."],
        details: "Runs the recovery loop until free space reaches the target, then stops. It \
                  only ever performs actions policy classified as needing no confirmation, so \
                  it cannot escalate into an ASK or PROTECTED resource to hit a number — if \
                  the target is unreachable that way, it reports that rather than reaching \
                  further. Holds the execution lock for the whole run, so it exits 75 if \
                  another invocation already holds it.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--target <N%|NB>",
                description: "Required. Either a percentage of the filesystem (`20%`) or an \
                              absolute byte count.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable.",
            },
        ],
        examples: &[ExampleSpec {
            command: "glomeris free --target 20%",
            purpose: "Reclaim until a fifth of the disk is free.",
        }],
        exit_codes: &[],
        see_also: &["clean", "execute", "emergency"],
    },
    CommandSpec {
        name: "emergency",
        group: Group::Act,
        safety: Safety::Destructive,
        summary: "Degraded-path recovery with no network and no LLM.",
        usage: &["emergency"],
        details: "For when the disk is full enough that the normal path cannot run. Takes no \
                  arguments, contacts no network, and uses no LLM — the recovery it performs \
                  is the same policy-gated set of no-confirmation-needed actions, chosen \
                  without any of the machinery that could itself need space. Holds the \
                  execution lock for the whole run, so it exits 75 if another invocation \
                  already holds it.",
        subcommands: &[],
        options: &[],
        examples: &[ExampleSpec {
            command: "glomeris emergency",
            purpose: "The disk is full and nothing else will run.",
        }],
        exit_codes: &[],
        see_also: &["free", "status"],
    },
    CommandSpec {
        name: "autopilot",
        group: Group::Act,
        safety: Safety::Destructive,
        summary: "Read, grant or revoke a bounded standing authorization.",
        usage: &[
            "autopilot <show|enable|revoke|run>",
            "[--kinds <tag,...>]",
            "[--max-actions <N>]",
            "[--max-bytes <N>]",
            "[--max-duration <secs>]",
            "[--min-pressure <state|none>]",
            "[--preauthorize-ask <kind>:<reason>]",
            "[--json]",
            "[--dry-run]",
            "[--plan-file <path>]",
            "[--project-root <path>]...",
        ],
        details: "A standing grant with limits, written down where you can read it. \
                  Only `run` can delete anything, and only what the grant already allowed: \
                  an envelope authorizes resource kinds, a number of actions, a byte total, \
                  a wall-clock budget and optionally a disk-pressure floor, and every \
                  candidate is still classified by policy and revalidated immediately before \
                  deletion. \
                  \n\nDefaults grant nothing: with no envelope file, Autopilot is revoked \
                  and `run` executes nothing. AUTO_SAFE resources are the only ones a grant \
                  reaches by default; PROTECTED refuses unconditionally and no flag here can \
                  change that; ASK refuses unless that exact kind and reason were \
                  pre-authorized. \
                  \n\nAn LLM's only possible influence is --plan-file, which reorders \
                  candidates this machine already found. It cannot add a candidate, choose \
                  an action, supply a path, or raise a limit. There is no live-provider mode: \
                  asking a model is `llm-plan`'s job, so no deletion here waits on a network \
                  call.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "show",
                description: "Print the stored envelope and where it lives. The default, so a \
                              bare `glomeris autopilot` reads rather than acts.",
            },
            OptionSpec {
                syntax: "enable",
                description: "Write a new envelope from the flags on this command line and \
                              turn Autopilot on. Requires --kinds. Replaces the previous \
                              envelope rather than adding to it, so one line states the whole \
                              grant.",
            },
            OptionSpec {
                syntax: "revoke",
                description: "Turn Autopilot off. Takes effect on the next run — every run \
                              re-reads the file, so there is nothing to restart. Limits are \
                              kept so a later enable cannot return with limits you never read.",
            },
            OptionSpec {
                syntax: "run",
                description: "Consider the discovered candidates within the envelope. Without \
                              --dry-run this deletes. Holds the execution lock, so it exits 75 \
                              if another invocation already holds it.",
            },
            OptionSpec {
                syntax: "--kinds <tag,...>",
                description: "For `enable`: the resource kinds the grant covers, by the tags \
                              `actions list` prints, comma-separated. `unknown` is refused.",
            },
            OptionSpec {
                syntax: "--max-actions <N>",
                description: "For `enable`: how many actions one run may perform. Defaults to \
                              3 and cannot exceed 25.",
            },
            OptionSpec {
                syntax: "--max-bytes <N>",
                description: "For `enable`: the byte total one run may reclaim. Defaults to \
                              5 GiB and cannot exceed 64 GiB.",
            },
            OptionSpec {
                syntax: "--max-duration <secs>",
                description: "For `enable`: the wall-clock budget for one run, in whole \
                              seconds. Defaults to 60 and cannot exceed 900.",
            },
            OptionSpec {
                syntax: "--min-pressure <state|none>",
                description: "For `enable`: refuse to run unless disk pressure is at least \
                              this state. `none` (the default) does not require any. \
                              Unobservable pressure fails any floor you set.",
            },
            OptionSpec {
                syntax: "--preauthorize-ask <kind>:<reason>",
                description: "For `enable`: narrowly authorize one ASK reason for one kind, \
                              e.g. node_modules:rebuild_cost_high. Repeatable. Every \
                              PROTECTED reason and every evidence-quality reason is refused \
                              here, so this cannot become a blanket consent.",
            },
            OptionSpec {
                syntax: "--json",
                description: "For `show`, `enable` and `revoke`: print the envelope as JSON — \
                              the grant, the hard ceilings, every choice `enable` would accept, \
                              and what no envelope can ever authorize. The same shape from all \
                              three, always describing what is in force after the command ran. \
                              This is what the menu-bar app reads.",
            },
            OptionSpec {
                syntax: "--dry-run",
                description: "For `run`: show the real bounded plan, including where a budget \
                              cuts it off, and delete nothing.",
            },
            OptionSpec {
                syntax: "--plan-file <path>",
                description: "For `run`: read an LLM plan (see `llm-plan --schema`) and use it \
                              to order candidates. Ordering is its entire authority.",
            },
            OptionSpec {
                syntax: "--project-root <path>",
                description: "Same meaning as for `detect`. Repeatable.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris autopilot",
                purpose: "What is Autopilot allowed to do right now?",
            },
            ExampleSpec {
                command: "glomeris autopilot enable --kinds node_modules \\\n    \
                          --max-actions 1 --max-bytes 1073741824",
                purpose: "Grant one narrow thing, and nothing else.",
            },
            ExampleSpec {
                command: "glomeris autopilot run --dry-run",
                purpose: "What would it do, before letting it do anything?",
            },
            ExampleSpec {
                command: "glomeris autopilot revoke",
                purpose: "Stop it, now.",
            },
        ],
        exit_codes: &[
            ExitCodeSpec {
                code: 0,
                meaning: "the run finished inside its envelope, including when it found \
                          nothing it was allowed to do.",
            },
            ExitCodeSpec {
                code: 1,
                meaning: "an action executed and failed, or the envelope file could not be \
                          read or written.",
            },
            ExitCodeSpec {
                code: 2,
                meaning: "usage error, including an unknown resource kind, a limit above its \
                          ceiling, or `enable` without --kinds.",
            },
            ExitCodeSpec {
                code: 3,
                meaning: "Autopilot is not enabled, so nothing was attempted.",
            },
            ExitCodeSpec {
                code: 75,
                meaning: "another glomeris invocation holds the execution lock.",
            },
        ],
        see_also: &["execute", "free", "llm-plan", "actions"],
    },
    // ----------------------------------------------------------------- Observe
    CommandSpec {
        name: "history",
        group: Group::Observe,
        safety: Safety::ReadOnly,
        summary: "Recorded disk-pressure transitions.",
        usage: &["history", "[--json]", "[--limit <N>]"],
        details: "A bounded tail of the pressure-state changes the monitor recorded. This is \
                  the machine's disk history, not a record of actions taken — for what was \
                  actually executed, use `actions history`.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "--limit <N>",
                description: "How many of the most recent entries to show.",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print a HistoryReport as JSON on stdout.",
            },
        ],
        examples: &[ExampleSpec {
            command: "glomeris history --limit 20",
            purpose: "Has this been getting worse?",
        }],
        exit_codes: &[],
        see_also: &["actions", "status"],
    },
    CommandSpec {
        name: "actions",
        group: Group::Observe,
        safety: Safety::ReadOnly,
        summary: "Registered actions, or the real-execution audit log.",
        usage: &["actions <list|history>", "[--json]", "[--limit <N>]"],
        details: "`actions list` shows every action this binary has registered — the closed \
                  set an LLM plan or an `execute` call can select from, and nothing else. \
                  `actions history` shows a bounded tail of the audit log of real executions: \
                  what ran, against what, and how it ended.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "list",
                description: "Show the registered actions and which resource kind each \
                              applies to.",
            },
            OptionSpec {
                syntax: "history",
                description: "Show a bounded tail of the real-execution audit log.",
            },
            OptionSpec {
                syntax: "--limit <N>",
                description: "For `history`: how many of the most recent records to show.",
            },
            OptionSpec {
                syntax: "--json",
                description: "Print the report as JSON on stdout.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris actions list",
                purpose: "What is Glomeris even able to do?",
            },
            ExampleSpec {
                command: "glomeris actions history --limit 10",
                purpose: "What has it actually done to this machine?",
            },
        ],
        exit_codes: &[],
        see_also: &["execute", "history"],
    },
    // ----------------------------------------------------------------- Service
    CommandSpec {
        name: "daemon",
        group: Group::Service,
        safety: Safety::ReadOnly,
        summary: "Install, remove, inspect or run the background monitor.",
        usage: &["daemon <install [--force]|uninstall|status [--json]|run>"],
        details: "Manages the launch agent that watches disk pressure and notifies you. The \
                  monitor observes and notifies; it does not delete anything on its own, so \
                  installing it cannot cost you data. `daemon run` is the foreground entry \
                  point launchd itself calls — you rarely need it by hand.",
        subcommands: &[],
        options: &[
            OptionSpec {
                syntax: "install [--force]",
                description: "Write and load the launch agent. --force overwrites an existing \
                              plist.",
            },
            OptionSpec {
                syntax: "uninstall",
                description: "Unload and remove the launch agent.",
            },
            OptionSpec {
                syntax: "status [--json]",
                description: "Whether the agent is installed, loaded and recently alive.",
            },
            OptionSpec {
                syntax: "run",
                description: "Run the monitor in the foreground. Normally launchd's job.",
            },
        ],
        examples: &[
            ExampleSpec {
                command: "glomeris daemon install",
                purpose: "Start watching disk pressure in the background.",
            },
            ExampleSpec {
                command: "glomeris daemon status --json",
                purpose: "Is the monitor actually running?",
            },
        ],
        exit_codes: &[],
        see_also: &["status", "history"],
    },
];

/// Looks up a subcommand by its literal name.
pub fn find_command(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Column at which the summary starts in the grouped command list. Wide
/// enough for the longest name (`llm-check`) plus breathing room, and chosen
/// so that name + summary stays inside 80 columns.
const SUMMARY_COLUMN: usize = 14;

/// The one-line usage shown on a bad command, and at the top of full help.
///
/// Deliberately not the aggregate of every subcommand's flag syntax. That
/// aggregate is what the pre-HORO-1311 help printed: thirteen lines up to 146
/// columns wide, in which a user who typed one wrong word had to find their
/// answer. A bad command needs a pointer, not a manual.
pub fn render_usage_line() -> String {
    "usage: glomeris <command> [options]".to_string()
}

/// What to print after the usage line when a command was not recognized.
pub fn render_unknown_command_hint() -> String {
    format!(
        "{}\n\nRun `glomeris --help` for the command list, or\n\
         `glomeris <command> --help` for one command.",
        render_usage_line()
    )
}

/// One command's `usage:` block, packed onto 80-column lines with a hanging
/// indent under the command name.
///
/// Shared by per-command help and by the usage error a subcommand prints when
/// its own flags were wrong, so those two cannot describe the same command
/// differently — which is AC 6 at the level a user actually meets it.
pub fn render_command_usage(command: &CommandSpec) -> String {
    const PREFIX: &str = "usage: glomeris ";
    let hanging = " ".repeat(PREFIX.len());

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for (index, segment) in command.usage.iter().enumerate() {
        let indent_len = if lines.is_empty() && current.is_empty() {
            PREFIX.len()
        } else {
            hanging.len()
        };
        if current.is_empty() {
            current.push_str(segment);
        } else if indent_len + current.len() + 1 + segment.len() <= 80 {
            current.push(' ');
            current.push_str(segment);
        } else {
            lines.push(current);
            current = (*segment).to_string();
        }
        if index + 1 == command.usage.len() && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
    }

    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            out.push_str(&format!("{PREFIX}{line}\n"));
        } else {
            out.push_str(&format!("{hanging}{line}\n"));
        }
    }
    out
}

/// Top-level `glomeris --help`.
///
/// Grouped, one line per command, with the consequence of each group stated
/// before its commands. Ends with the two pointers that make this progressive
/// rather than partial: per-command help, and the exit-code reference.
pub fn render_top_level_help(version: &str) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "glomeris {version} — evidence-first developer storage cleanup.\n\n"
    ));
    // Wrapped rather than hand-broken: a hand-broken paragraph is how the
    // 80-column guarantee gets lost the next time someone edits a word in it.
    out.push_str(&wrap(
        "Finds reclaimable developer storage, shows the evidence for why each candidate is \
         safe or not, and acts only under policy control. AI can recommend; policy decides; \
         the executor verifies.",
        78,
        "",
    ));
    out.push_str("\n\n");
    out.push_str(&render_usage_line());
    out.push('\n');

    for group in Group::ALL {
        out.push_str(&format!("\n{} — {}\n", group.title(), group.caption()));
        for command in COMMANDS.iter().filter(|c| c.group == *group) {
            let padding = SUMMARY_COLUMN.saturating_sub(command.name.len());
            out.push_str(&format!(
                "  {}{}{}\n",
                command.name,
                " ".repeat(padding),
                command.summary
            ));
        }
    }

    out.push_str("\nStart here\n");
    for example in FIRST_STEPS {
        out.push_str(&format!("  {}\n", example.command));
        out.push_str(&format!("  {}{}\n", " ".repeat(4), example.purpose));
    }

    out.push_str("\nMore\n");
    for (invocation, description) in MORE_POINTERS {
        let padding = MORE_COLUMN.saturating_sub(invocation.len());
        out.push_str(&format!(
            "  {}{}{}\n",
            invocation,
            " ".repeat(padding),
            description
        ));
    }

    out
}

/// Where top-level help sends a reader who wants more than one line.
///
/// This footer is the whole progressive-disclosure contract: top-level help
/// owes the reader an accurate map and a way down, not the full reference.
const MORE_POINTERS: &[(&str, &str)] = &[
    (
        "glomeris <command> --help",
        "Options, examples, and what it will not do.",
    ),
    ("glomeris help exit-codes", "What each exit status means."),
];

/// Column at which `MORE_POINTERS` descriptions start.
const MORE_COLUMN: usize = 28;

/// The four commands a first-time user should run, in order, and the question
/// each answers.
///
/// Four rather than one per group: the point is a path through the product,
/// and the path a new user needs is "see the problem, see the candidates,
/// understand one of them, see what would happen" — all of it before anything
/// is deleted. The destructive commands are reachable from the group list
/// above; they do not belong in a section a stranger reads first.
const FIRST_STEPS: &[ExampleSpec] = &[
    ExampleSpec {
        command: "glomeris status",
        purpose: "Is anything wrong right now?",
    },
    ExampleSpec {
        command: "glomeris detect",
        purpose: "What could I reclaim, and how is each one classified?",
    },
    ExampleSpec {
        command: "glomeris explain <resource_id_or_path>",
        purpose: "Why is that one safe, or not?",
    },
    ExampleSpec {
        command: "glomeris clean --dry-run",
        purpose: "What exactly would be removed? (Still removes nothing.)",
    },
];

/// The safety line under a command's usage block (HORO-1485).
///
/// One label when every way into the command has the same consequence — which
/// is every flags-only command, and `actions`, whose two verbs both only read.
/// When the verbs differ, a single label would have to be either the weakest
/// (a false reassurance: `daemon`'s "changes nothing" over `install`) or the
/// strongest (a false warning: "can delete data" over `autopilot show`), so
/// instead the line says the safety is per verb, states the strongest, and
/// sends the reader to the block that labels each one. Nothing here claims
/// anything about a verb that is not true of it.
fn render_safety_statement(command: &CommandSpec) -> String {
    let strongest = command
        .subcommands
        .iter()
        .map(|s| s.safety)
        .fold(command.safety, Safety::max);

    let uniform = command
        .subcommands
        .iter()
        .all(|s| s.safety == command.safety);

    if uniform {
        format!("{}\n", strongest.label())
    } else {
        format!(
            "{}\n",
            wrap(
                &format!(
                    "Safety depends on the subcommand; each is labelled below. The strongest \
                     of them: {}",
                    strongest.label()
                ),
                78,
                "",
            )
        )
    }
}

/// Per-command `glomeris <command> --help`.
pub fn render_command_help(command: &CommandSpec) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "glomeris {} — {}\n\n",
        command.name, command.summary
    ));
    out.push_str(&render_command_usage(command));
    out.push('\n');
    out.push_str(&render_safety_statement(command));
    out.push('\n');
    out.push_str(&format!("{}\n", wrap(command.details, 78, "")));

    if !command.subcommands.is_empty() {
        out.push_str("\nSubcommands\n");
        for sub in command.subcommands {
            out.push_str(&format!("  {}\n", sub.syntax()));
            out.push_str(&format!("{}\n", wrap(sub.safety.label(), 74, "      ")));
            out.push_str(&format!("{}\n", wrap(sub.description, 74, "      ")));
        }
    }

    if !command.options.is_empty() {
        out.push_str("\nOptions\n");
        for option in command.options {
            out.push_str(&format!("  {}\n", option.syntax));
            out.push_str(&format!("{}\n", wrap(option.description, 74, "      ")));
        }
    }

    if !command.examples.is_empty() {
        out.push_str("\nExamples\n");
        for example in command.examples {
            out.push_str(&format!("  {}\n", example.command));
            out.push_str(&format!("      {}\n", example.purpose));
        }
    }

    if !command.exit_codes.is_empty() {
        out.push_str("\nExit codes\n");
        for spec in command.exit_codes {
            out.push_str(&format!(
                "  {:<4}{}\n",
                spec.code,
                wrap_tail(spec.meaning, 74)
            ));
        }
        out.push_str("  Run `glomeris help exit-codes` for the codes shared by every command.\n");
    }

    if !command.see_also.is_empty() {
        out.push_str(&format!("\nSee also: {}\n", command.see_also.join(", ")));
    }

    out
}

/// `glomeris help exit-codes` — AC 7's progressive disclosure.
///
/// A separate topic rather than a section of top-level help, because exit
/// codes are what you need on your second day and never on your first. The
/// shared meanings live here; the codes that mean something specific to one
/// command live in that command's own help, where they cannot be mistaken for
/// universal.
pub fn render_exit_codes() -> String {
    let mut out = String::from(
        "Exit codes\n\n\
         Shared by every subcommand:\n\
        \x20 0   success.\n\
        \x20 1   a real failure: a macOS-only command on another platform, a platform\n\
        \x20     operation that failed, or a provider call that could not complete.\n\
        \x20 2   usage error: an unknown command or subcommand, a missing or\n\
        \x20     unrecognized argument, or a credential passed as a flag.\n\
        \x20 75  the execution lock is held by another glomeris invocation. Only the\n\
        \x20     commands that can delete take this lock.\n\n\
         Commands that give these codes more specific meanings:\n",
    );

    for command in COMMANDS.iter().filter(|c| !c.exit_codes.is_empty()) {
        let codes = command
            .exit_codes
            .iter()
            .map(|spec| spec.code.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  {:<12}{:<22}", command.name, codes));
        out.push_str(&format!("`glomeris {} --help`\n", command.name));
    }

    out.push_str(
        "\n`execute` is the one worth reading in full before scripting against it: its\n\
         codes distinguish a policy refusal from a revalidation abort from a plan that\n\
         ran and failed, which a caller needs to tell apart.\n",
    );

    out
}

/// The named `glomeris help <topic>` topics.
///
/// One topic today. It is a table rather than an `if` so that adding the
/// second one cannot reintroduce the drift this module exists to prevent —
/// `help` with no topic lists exactly what `help <topic>` accepts.
pub const HELP_TOPICS: &[(&str, &str)] = &[(
    "exit-codes",
    "What each exit status means, and which are command-specific.",
)];

/// What `glomeris help <unknown-topic>` prints.
pub fn render_topic_list() -> String {
    let mut out = String::from("usage: glomeris help <topic>\n\nTopics\n");
    for (name, description) in HELP_TOPICS {
        out.push_str(&format!("  {:<14}{}\n", name, description));
    }
    out.push_str("\nFor a command rather than a topic, run `glomeris <command> --help`.\n");
    out
}

/// Greedy word wrap to `width` columns, prefixing every line with `indent`.
///
/// Hand-rolled rather than pulled in as a dependency: the whole requirement is
/// "do not emit a 146-column line", and a crate for that would be a new
/// supply-chain edge for thirty lines of logic. `\n\n` in the input is
/// honoured as a paragraph break, which is the only formatting any of the
/// strings above need.
fn wrap(text: &str, width: usize, indent: &str) -> String {
    let usable = width.saturating_sub(indent.len()).max(20);
    let mut paragraphs = Vec::new();

    for paragraph in text.split("\n\n") {
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.len() + 1 + word.len() <= usable {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(format!("{indent}{current}"));
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(format!("{indent}{current}"));
        }
        paragraphs.push(lines.join("\n"));
    }

    paragraphs.join("\n\n")
}

/// Wraps a continuation-indented tail, for the exit-code rows where the first
/// line already sits after a code column.
fn wrap_tail(text: &str, width: usize) -> String {
    let wrapped = wrap(text, width, "      ");
    wrapped.trim_start().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC 1's measurable half. The pre-HORO-1311 help was 146 columns wide,
    /// which wraps into unreadable ragged blocks in any normal terminal.
    #[test]
    fn no_help_output_exceeds_eighty_columns() {
        let mut rendered = vec![
            ("top-level".to_string(), render_top_level_help("0.2.0")),
            ("exit-codes".to_string(), render_exit_codes()),
            ("topics".to_string(), render_topic_list()),
            ("usage".to_string(), render_unknown_command_hint()),
        ];
        for command in COMMANDS {
            rendered.push((command.name.to_string(), render_command_help(command)));
        }

        for (label, text) in rendered {
            for line in text.lines() {
                assert!(
                    line.chars().count() <= 80,
                    "{label} help has a {}-column line: {line}",
                    line.chars().count()
                );
            }
        }
    }

    /// The other half of AC 1: it has to fit a screen, not just a width.
    #[test]
    fn top_level_help_fits_a_terminal_screen() {
        let lines = render_top_level_help("0.2.0").lines().count();
        assert!(
            lines <= 48,
            "top-level help is {lines} lines; a reader should not have to scroll to see the \
             command list"
        );
    }

    #[test]
    fn every_command_belongs_to_a_rendered_group() {
        for command in COMMANDS {
            assert!(
                Group::ALL.contains(&command.group),
                "{} is in a group the top-level help never renders",
                command.name
            );
        }
    }

    /// A dangling `see_also` is worse than none: it sends a user to type a
    /// command that does not exist and conclude the tool is broken.
    #[test]
    fn every_see_also_names_a_real_command() {
        for command in COMMANDS {
            for sibling in command.see_also {
                assert!(
                    find_command(sibling).is_some(),
                    "{} points at nonexistent command '{sibling}'",
                    command.name
                );
                assert_ne!(
                    *sibling, command.name,
                    "{} lists itself under See also",
                    command.name
                );
            }
        }
    }

    /// The usage block is what a user copies. If its first segment does not
    /// start with the command's own name, the copied line does not run.
    #[test]
    fn every_usage_fragment_starts_with_its_command_name() {
        for command in COMMANDS {
            let first = command
                .usage
                .first()
                .unwrap_or_else(|| panic!("{} has an empty usage block", command.name));
            assert!(
                first.starts_with(command.name),
                "{}'s usage block starts with something else: {first}",
                command.name
            );
        }
    }

    /// A segment wider than the line budget cannot be packed, so it would be
    /// emitted whole and blow the width guarantee. Caught here, at the data,
    /// rather than as a mysterious failure in the rendered-width test.
    #[test]
    fn no_usage_segment_is_too_wide_to_pack() {
        let budget = 80 - "usage: glomeris ".len();
        for command in COMMANDS {
            for segment in command.usage {
                assert!(
                    segment.len() <= budget,
                    "{}'s usage segment is {} chars, over the {budget}-char budget: {segment}",
                    command.name,
                    segment.len()
                );
            }
        }
    }

    /// Both places a usage line appears render it from the same function, so a
    /// subcommand's flag-error message and its `--help` cannot disagree.
    #[test]
    fn per_command_help_embeds_the_same_usage_block_it_prints_on_an_error() {
        for command in COMMANDS {
            let usage = render_command_usage(command);
            assert!(
                render_command_help(command).contains(&usage),
                "{}'s help does not contain the usage block it would print on a flag error",
                command.name
            );
        }
    }

    /// AC 3. Every command states its consequence class, and the destructive
    /// ones are exactly the ones that can delete — asserted as a set so that
    /// classifying a new command wrongly, or forgetting to, fails here rather
    /// than in a user's terminal.
    ///
    /// `autopilot` is in this list (HORO-1310) because `autopilot run` without
    /// `--dry-run` deletes. Three of its four verbs cannot, but a command's
    /// safety class has to describe the worst thing typing its name can do,
    /// not the most common thing.
    #[test]
    fn exactly_the_deleting_commands_are_marked_destructive() {
        let destructive: Vec<&str> = COMMANDS
            .iter()
            .filter(|c| c.safety == Safety::Destructive)
            .map(|c| c.name)
            .collect();

        assert_eq!(
            destructive,
            vec!["execute", "free", "emergency", "autopilot"]
        );
    }

    /// AC 4, stated as a property rather than as a substring of one sentence:
    /// the planner is in the advisory group, is marked advisory, and its help
    /// says so in words a user reads before running it.
    #[test]
    fn llm_plan_is_advisory_everywhere_it_is_described() {
        let spec = find_command("llm-plan").expect("llm-plan must be registered");

        assert_eq!(spec.group, Group::Plan);
        assert_eq!(spec.safety, Safety::Advisory);
        assert!(spec.summary.contains("Advisory"));
        assert!(
            spec.details.contains("Nothing a model returns is executed"),
            "details: {}",
            spec.details
        );
    }

    /// AC 5. Each of the three names its confirmation and policy behaviour,
    /// and `execute` in particular says the thing no flag can change.
    #[test]
    fn the_destructive_commands_explain_their_policy_behaviour() {
        let execute = find_command("execute").expect("execute must be registered");
        assert!(execute
            .details
            .contains("PROTECTED refuses unconditionally"));
        assert!(execute.details.contains("--confirm-ask"));
        assert!(execute
            .details
            .contains("AUTO_SAFE proceeds with no confirmation"));

        let free = find_command("free").expect("free must be registered");
        assert!(free.details.contains("cannot escalate"));

        let emergency = find_command("emergency").expect("emergency must be registered");
        assert!(emergency.details.contains("policy-gated"));
    }

    /// The safety axis must not be worded as if it were the policy axis. A
    /// command is read-only or not; a *resource* is AUTO_SAFE, ASK or
    /// PROTECTED. Conflating them is the specific confusion this campaign's
    /// design direction rules out, and the easiest place to introduce it is a
    /// one-line label.
    #[test]
    fn safety_labels_never_borrow_policy_vocabulary() {
        for safety in [Safety::ReadOnly, Safety::Advisory, Safety::Destructive] {
            let label = safety.label();
            for policy_token in ["AUTO_SAFE", "ASK", "PROTECTED"] {
                assert!(
                    !label.contains(policy_token),
                    "{label:?} describes a command using a resource's policy class"
                );
            }
        }
    }

    /// Wrapping is the mechanism the width guarantee rests on, so it is worth
    /// one direct test rather than only being covered through rendered output.
    #[test]
    fn wrap_breaks_on_words_and_keeps_paragraphs() {
        assert_eq!(
            wrap("aaaa bbbb cccc dddd eeee", 22, ""),
            "aaaa bbbb cccc dddd\neeee"
        );

        assert_eq!(
            wrap("alpha beta gamma delta epsilon", 28, "    "),
            "    alpha beta gamma delta\n    epsilon"
        );

        assert_eq!(wrap("a\n\nb", 40, ""), "a\n\nb");
    }

    /// The usable width has a 20-column floor, so a deeply indented block
    /// cannot collapse into one-word-per-line. Asserted rather than left
    /// implicit because it means a `width` under 20 is not honoured — which
    /// would otherwise look like a wrapping bug to the next reader.
    #[test]
    fn wrap_does_not_honour_a_width_below_its_floor() {
        assert_eq!(wrap("aaaa bbbb cccc dddd", 8, ""), "aaaa bbbb cccc dddd");
    }

    /// A word longer than the usable width cannot be broken without lying
    /// about the syntax, so it must be emitted whole rather than truncated or
    /// hyphenated. Documented by test because the alternative — silently
    /// splitting a long resource id — would be worse than a long line.
    #[test]
    fn wrap_never_splits_a_single_long_word() {
        let long = "cargo_target_dir:/Users/someone/projects/deeply/nested/target";
        assert_eq!(wrap(long, 20, ""), long);
    }
}
