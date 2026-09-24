//! Persistence for the Autopilot envelope (HORO-1310).
//!
//! The envelope is the one piece of Glomeris state that *grants authority*,
//! so how it is read back matters as much as what it contains. Two rules
//! follow from that, and they are the whole design of this module:
//!
//! 1. **Loading goes through the validating constructors.** Every value
//!    read out of the file is handed to
//!    [`AutopilotEnvelope::allow_kind`] / [`AutopilotEnvelope::preauthorize_ask`]
//!    / `set_max_*`, which enforce the hard ceilings and the
//!    pre-authorizable-reason rule. There is deliberately **no**
//!    `Deserialize` impl for [`AutopilotEnvelope`]: one would reconstruct
//!    the private fields directly and walk straight past all of it. A
//!    hand-edited file, or one written by the SwiftUI client, therefore
//!    cannot grant more than the CLI itself can — it can only fail to load.
//!    (Same reasoning as [`crate::actions`]' rule about never deriving
//!    `Deserialize` on a domain type.)
//! 2. **Unknown keys are an error, not noise.** Mirrors
//!    `#[serde(deny_unknown_fields)]` on [`crate::actions::llm::LlmPlan`]:
//!    on an authority-granting input, a line nobody understands must fail
//!    loudly rather than be skipped. A typo'd `max_byte = 1` that silently
//!    left the byte budget at its default would be the worst outcome here.
//!
//! A missing file loads as [`AutopilotEnvelope::revoked`] — absence of an
//! envelope means no authority, and Autopilot does not require a file to
//! exist in order to be off. A *malformed* file is an error rather than a
//! silent fall back to revoked: refusing to run is safe either way, and an
//! error is the only one of the two that tells the user their file is
//! broken.
//!
//! ## File format
//!
//! `key = value`, one per line; `#` comments and blank lines ignored;
//! `preauthorize_ask` may repeat, every other key at most once.
//!
//! ```text
//! version = 1
//! enabled = true
//! allowed_kinds = cargo_target_dir, xcode_derived_data
//! max_actions = 3
//! max_bytes = 5368709120
//! max_duration_secs = 60
//! min_pressure = PRESSURED
//! preauthorize_ask = cargo_target_dir:rebuild_cost_high
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::evidence::ResourceKind;
use crate::monitor::PressureState;
use crate::policy::ReasonCode;

use super::envelope::{AutopilotEnvelope, EnvelopeError};

/// The only file-format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// How much of an offending value an error message quotes. The envelope
/// file holds tags and integers and never a credential (the API key lives
/// in the Keychain — see `scripts/check-credential-store-uses-keychain.sh`),
/// but an error string can end up in a log, so a malformed line is quoted
/// only far enough to be recognizable.
const MAX_QUOTED_VALUE: usize = 40;

/// Why loading or saving an envelope failed.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// The file declares a `version` this build does not understand.
    /// Refused rather than best-effort parsed: a future version might make
    /// a key *mean* something narrower, and reading it under this build's
    /// rules could widen authority.
    UnsupportedVersion {
        found: String,
    },
    /// The file has no `version` line at all.
    MissingVersion,
    /// A line is not a comment, not blank, and not `key = value`.
    MalformedLine {
        line: usize,
    },
    /// A key this build does not know. See rule 2 in the module docs.
    UnknownKey {
        line: usize,
        key: String,
    },
    /// A single-valued key appeared twice.
    DuplicateKey {
        line: usize,
        key: String,
    },
    /// A value the key's own parser rejected (a bad integer, an unknown
    /// tag, a malformed `kind:reason` pair).
    InvalidValue {
        line: usize,
        key: &'static str,
        value: String,
    },
    /// A value that parsed, but that [`AutopilotEnvelope`] refused — a
    /// budget above its ceiling, `unknown` in the allowlist, a reason no
    /// envelope may pre-authorize. This is rule 1 doing its job.
    Refused {
        line: usize,
        source: EnvelopeError,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::UnsupportedVersion { found } => write!(
                f,
                "unsupported Autopilot envelope version {found:?} \
                 (this build understands version {FORMAT_VERSION})"
            ),
            StoreError::MissingVersion => write!(
                f,
                "the Autopilot envelope file has no `version` line \
                 (expected `version = {FORMAT_VERSION}`)"
            ),
            StoreError::MalformedLine { line } => {
                write!(
                    f,
                    "line {line}: expected `key = value`, a comment, or a blank line"
                )
            }
            StoreError::UnknownKey { line, key } => {
                write!(f, "line {line}: unknown Autopilot envelope key {key:?}")
            }
            StoreError::DuplicateKey { line, key } => {
                write!(f, "line {line}: {key:?} is set more than once")
            }
            StoreError::InvalidValue { line, key, value } => {
                write!(f, "line {line}: {value:?} is not a valid `{key}`")
            }
            StoreError::Refused { line, source } => {
                write!(f, "line {line}: {source}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io(e) => Some(e),
            StoreError::Refused { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// Resolves the per-user envelope path, using `$HOME` — same
/// `~/Library/Application Support/Glomeris/` directory and same
/// missing-`HOME` handling as
/// [`crate::executor::lock::default_lock_path`].
pub fn default_envelope_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("autopilot.conf"))
}

/// Loads the envelope from [`default_envelope_path`].
pub fn load_envelope() -> Result<AutopilotEnvelope, StoreError> {
    load_envelope_at(&default_envelope_path()?)
}

/// Saves `envelope` to [`default_envelope_path`].
pub fn save_envelope(envelope: &AutopilotEnvelope) -> Result<(), StoreError> {
    save_envelope_at(&default_envelope_path()?, envelope)
}

/// Loads the envelope from `path`, or [`AutopilotEnvelope::revoked`] if no
/// file is there.
///
/// Every run must call this immediately before acting rather than caching
/// the result, so that revoking Autopilot — from the CLI or from the GUI —
/// takes effect on the very next run with no daemon restart and no
/// invalidation step to forget.
pub fn load_envelope_at(path: &Path) -> Result<AutopilotEnvelope, StoreError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AutopilotEnvelope::revoked()),
        Err(e) => return Err(StoreError::Io(e)),
    };
    parse_envelope(&contents)
}

/// One `key = value` line's worth of already-parsed input, before anything
/// is handed to [`AutopilotEnvelope`].
struct RawEnvelope {
    version: Option<(usize, String)>,
    enabled: Option<(usize, bool)>,
    allowed_kinds: Option<(usize, Vec<ResourceKind>)>,
    max_actions: Option<(usize, u32)>,
    max_bytes: Option<(usize, u64)>,
    max_duration: Option<(usize, Duration)>,
    min_pressure: Option<(usize, Option<PressureState>)>,
    preauthorize_ask: Vec<(usize, ResourceKind, ReasonCode)>,
}

/// Parses an envelope file's contents.
///
/// Two passes on purpose. The first only reads lines; the second applies
/// them to a [`AutopilotEnvelope::revoked`] envelope in a fixed order —
/// allowlist, then pre-authorizations (which require their kind to be
/// allowlisted already), then budgets, and `enable` strictly last. That
/// makes the file order-insensitive without making the *application* order
/// depend on it, so a file that happens to list `preauthorize_ask` above
/// `allowed_kinds` is not quietly refused.
fn parse_envelope(contents: &str) -> Result<AutopilotEnvelope, StoreError> {
    let raw = read_lines(contents)?;

    let Some((_, version)) = raw.version else {
        return Err(StoreError::MissingVersion);
    };
    if version != FORMAT_VERSION.to_string() {
        return Err(StoreError::UnsupportedVersion { found: version });
    }

    let mut envelope = AutopilotEnvelope::revoked();

    if let Some((line, kinds)) = raw.allowed_kinds {
        for kind in kinds {
            envelope
                .allow_kind(kind)
                .map_err(|source| StoreError::Refused { line, source })?;
        }
    }
    for (line, kind, reason) in raw.preauthorize_ask {
        envelope
            .preauthorize_ask(kind, reason)
            .map_err(|source| StoreError::Refused { line, source })?;
    }
    if let Some((line, max_actions)) = raw.max_actions {
        envelope
            .set_max_actions(max_actions)
            .map_err(|source| StoreError::Refused { line, source })?;
    }
    if let Some((line, max_bytes)) = raw.max_bytes {
        envelope
            .set_max_bytes(max_bytes)
            .map_err(|source| StoreError::Refused { line, source })?;
    }
    if let Some((line, max_duration)) = raw.max_duration {
        envelope
            .set_max_duration(max_duration)
            .map_err(|source| StoreError::Refused { line, source })?;
    }
    if let Some((_, min_pressure)) = raw.min_pressure {
        envelope.set_min_pressure(min_pressure);
    }
    if raw.enabled.is_some_and(|(_, enabled)| enabled) {
        envelope.enable();
    }

    Ok(envelope)
}

fn read_lines(contents: &str) -> Result<RawEnvelope, StoreError> {
    let mut raw = RawEnvelope {
        version: None,
        enabled: None,
        allowed_kinds: None,
        max_actions: None,
        max_bytes: None,
        max_duration: None,
        min_pressure: None,
        preauthorize_ask: Vec::new(),
    };

    for (index, raw_line) in contents.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(StoreError::MalformedLine { line });
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "version" => set_once(&mut raw.version, line, key, value.to_string())?,
            "enabled" => set_once(&mut raw.enabled, line, key, parse_bool(line, value)?)?,
            "allowed_kinds" => {
                set_once(&mut raw.allowed_kinds, line, key, parse_kinds(line, value)?)?
            }
            "max_actions" => set_once(
                &mut raw.max_actions,
                line,
                key,
                parse_int::<u32>(line, "max_actions", value)?,
            )?,
            "max_bytes" => set_once(
                &mut raw.max_bytes,
                line,
                key,
                parse_int::<u64>(line, "max_bytes", value)?,
            )?,
            "max_duration_secs" => set_once(
                &mut raw.max_duration,
                line,
                key,
                Duration::from_secs(parse_int::<u64>(line, "max_duration_secs", value)?),
            )?,
            "min_pressure" => set_once(
                &mut raw.min_pressure,
                line,
                key,
                parse_pressure(line, value)?,
            )?,
            "preauthorize_ask" => {
                let (kind, reason) = parse_preauthorization(line, value)?;
                raw.preauthorize_ask.push((line, kind, reason));
            }
            _ => {
                return Err(StoreError::UnknownKey {
                    line,
                    key: quote_value(key),
                })
            }
        }
    }

    Ok(raw)
}

fn set_once<T>(
    slot: &mut Option<(usize, T)>,
    line: usize,
    key: &str,
    value: T,
) -> Result<(), StoreError> {
    if slot.is_some() {
        return Err(StoreError::DuplicateKey {
            line,
            key: key.to_string(),
        });
    }
    *slot = Some((line, value));
    Ok(())
}

fn invalid(line: usize, key: &'static str, value: &str) -> StoreError {
    StoreError::InvalidValue {
        line,
        key,
        value: quote_value(value),
    }
}

/// Truncates an offending value for an error message. See
/// [`MAX_QUOTED_VALUE`].
fn quote_value(value: &str) -> String {
    if value.chars().count() <= MAX_QUOTED_VALUE {
        return value.to_string();
    }
    let head: String = value.chars().take(MAX_QUOTED_VALUE).collect();
    format!("{head}…")
}

/// Exactly `true` or `false`. No `yes`/`1`/`on`, and no case folding: this
/// is the bit that turns Autopilot on.
fn parse_bool(line: usize, value: &str) -> Result<bool, StoreError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(line, "enabled", value)),
    }
}

fn parse_int<T: std::str::FromStr>(
    line: usize,
    key: &'static str,
    value: &str,
) -> Result<T, StoreError> {
    value.parse::<T>().map_err(|_| invalid(line, key, value))
}

/// A comma-separated list of [`ResourceKind::tag`] values. An empty list is
/// allowed and means "nothing is allowlisted", which is what a revoked
/// envelope looks like.
fn parse_kinds(line: usize, value: &str) -> Result<Vec<ResourceKind>, StoreError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|tag| {
            let tag = tag.trim();
            ResourceKind::from_tag(tag).ok_or_else(|| invalid(line, "allowed_kinds", tag))
        })
        .collect()
}

/// A [`PressureState::as_str`] tag, or `none` for "no floor".
fn parse_pressure(line: usize, value: &str) -> Result<Option<PressureState>, StoreError> {
    if value == "none" {
        return Ok(None);
    }
    PressureState::ALL
        .into_iter()
        .find(|state| state.as_str() == value)
        .map(Some)
        .ok_or_else(|| invalid(line, "min_pressure", value))
}

/// `<resource_kind_tag>:<reason_code_tag>`. Both halves are parsed
/// strictly; whether the pair is *allowed* is
/// [`AutopilotEnvelope::preauthorize_ask`]'s call, not this function's.
fn parse_preauthorization(
    line: usize,
    value: &str,
) -> Result<(ResourceKind, ReasonCode), StoreError> {
    let Some((kind, reason)) = value.split_once(':') else {
        return Err(invalid(line, "preauthorize_ask", value));
    };
    let kind = ResourceKind::from_tag(kind.trim())
        .ok_or_else(|| invalid(line, "preauthorize_ask", value))?;
    let reason = ReasonCode::from_tag(reason.trim())
        .ok_or_else(|| invalid(line, "preauthorize_ask", value))?;
    Ok((kind, reason))
}

/// Renders `envelope` in the format [`parse_envelope`] reads.
///
/// `max_duration` is written as whole seconds, truncating — the CLI's
/// `--max-duration` is in seconds, and truncation errs toward the shorter,
/// narrower budget.
pub fn render_envelope(envelope: &AutopilotEnvelope) -> String {
    let mut out = String::new();
    out.push_str("# Glomeris Autopilot envelope.\n");
    out.push_str("# Written by `glomeris autopilot`. Safe to read; edits are re-validated\n");
    out.push_str("# on load and cannot grant more than the CLI itself can.\n");
    out.push_str(&format!("version = {FORMAT_VERSION}\n"));
    out.push_str(&format!("enabled = {}\n", envelope.is_enabled()));
    let kinds: Vec<&str> = envelope
        .allowed_kinds()
        .iter()
        .map(|kind| kind.tag())
        .collect();
    out.push_str(&format!("allowed_kinds = {}\n", kinds.join(", ")));
    out.push_str(&format!("max_actions = {}\n", envelope.max_actions()));
    out.push_str(&format!("max_bytes = {}\n", envelope.max_bytes()));
    out.push_str(&format!(
        "max_duration_secs = {}\n",
        envelope.max_duration().as_secs()
    ));
    out.push_str(&format!(
        "min_pressure = {}\n",
        match envelope.min_pressure() {
            Some(state) => state.as_str(),
            None => "none",
        }
    ));
    for preauthorization in envelope.ask_preauthorizations() {
        out.push_str(&format!(
            "preauthorize_ask = {}:{}\n",
            preauthorization.kind().tag(),
            preauthorization.reason().as_str()
        ));
    }
    out
}

/// Writes `envelope` to `path`, creating parent directories as needed.
///
/// Writes through [`crate::atomic_write::write_atomically`] — a temp file
/// beside `path`, then `fs::rename` over it — for a sharper reason here
/// than anywhere else this crate writes a file: the GUI and the CLI both
/// write this one, and a reader that caught a half-written envelope would
/// see neither the old nor the new grant.
///
/// Which is what the write itself could produce until HORO-1464. Two
/// writers sharing a temp path derived from the target truncated each
/// other's scratch file and renamed the mixture into place, so the atomic
/// rename published exactly the envelope this comment says cannot be
/// observed.
///
/// What that could and could not do, stated so the severity is not read as
/// larger than it is: [`parse_envelope`] builds up from
/// [`AutopilotEnvelope::revoked`] and only ever *adds* grants from lines it
/// reads, so a torn envelope grants less, and an unparseable one is an
/// error — fail-closed either way. The exception is the caps. `max_actions`,
/// `max_bytes` and `max_duration` each fall back to their built-in default
/// when their line is absent, so a tear that drops a cap line while keeping
/// the grant lines widens that one cap back to the default, which is looser
/// than any value the user had tightened it to.
pub fn save_envelope_at(path: &Path, envelope: &AutopilotEnvelope) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_write::write_atomically(path, render_envelope(envelope).as_bytes())
        .map_err(StoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::super::envelope::{ACTIONS_CEILING, BYTES_CEILING, DURATION_CEILING};
    use super::*;
    use crate::atomic_write::temp_paths_beside;

    fn load(contents: &str) -> Result<AutopilotEnvelope, StoreError> {
        parse_envelope(contents)
    }

    fn narrow() -> AutopilotEnvelope {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        envelope
            .allow_kind(ResourceKind::CargoTargetDir)
            .expect("allowlistable");
        envelope
            .allow_kind(ResourceKind::XcodeDerivedData)
            .expect("allowlistable");
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .expect("pre-authorizable");
        envelope.set_max_actions(2).expect("below the ceiling");
        envelope.set_max_bytes(1_024).expect("below the ceiling");
        envelope
            .set_max_duration(Duration::from_secs(30))
            .expect("below the ceiling");
        envelope.set_min_pressure(Some(PressureState::Warn));
        envelope
    }

    #[test]
    fn a_missing_file_loads_as_a_revoked_envelope() {
        let dir = std::env::temp_dir().join("glomeris-autopilot-store-missing");
        let path = dir.join("definitely-not-written.conf");
        let _ = std::fs::remove_file(&path);

        let envelope = load_envelope_at(&path).expect("a missing file is not an error");

        assert_eq!(envelope, AutopilotEnvelope::revoked());
        assert!(!envelope.is_enabled());
    }

    #[test]
    fn an_envelope_round_trips_through_render_and_parse() {
        let original = narrow();
        let reloaded = load(&render_envelope(&original)).expect("its own output must parse");
        assert_eq!(reloaded, original);
    }

    #[test]
    fn a_revoked_envelope_round_trips_too() {
        let original = AutopilotEnvelope::revoked();
        let reloaded = load(&render_envelope(&original)).expect("its own output must parse");
        assert_eq!(reloaded, original);
    }

    #[test]
    fn save_then_load_round_trips_through_a_real_file() {
        let dir = std::env::temp_dir().join("glomeris-autopilot-store-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("autopilot.conf");
        let original = narrow();

        save_envelope_at(&path, &original).expect("save");
        let reloaded = load_envelope_at(&path).expect("load");

        assert_eq!(reloaded, original);
        // Scanned rather than named: since HORO-1464 the temp name carries
        // this process's id and a sequence number, so asserting one fixed
        // path would name a file that can never exist — a pass that tests
        // nothing.
        assert_eq!(
            temp_paths_beside(&path),
            Vec::<PathBuf>::new(),
            "the temp file must not survive a successful save"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pins the wiring: the envelope must go through `atomic_write`, whose
    /// own tests prove a writer never truncates a file it did not create.
    /// The pre-placed file sits at the name the old code derived from the
    /// target, standing in for the other of the two writers this file's own
    /// doc comment says to expect — the GUI while the CLI saves, or the
    /// reverse. Fixed, known name, so no timing is involved.
    #[test]
    fn saving_the_envelope_cannot_disturb_a_file_at_the_old_predictable_temp_name() {
        const SQUATTER: &str = "version = 1\nenabled = false\n# the other writer's scratch file\n";
        let dir = std::env::temp_dir().join(format!(
            "glomeris-autopilot-store-old-temp-name-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        let path = dir.join("autopilot.conf");
        let old_temp_name = path.with_extension("conf.tmp");
        std::fs::write(&old_temp_name, SQUATTER).expect("place the squatter");
        let original = narrow();

        save_envelope_at(&path, &original).expect("save");

        assert_eq!(load_envelope_at(&path).expect("load"), original);
        assert_eq!(
            std::fs::read_to_string(&old_temp_name).expect("still readable"),
            SQUATTER,
            "the save must not have used — and so must not have truncated — \
             the temp name it derived from the target before HORO-1464"
        );
        assert_eq!(temp_paths_beside(&path), Vec::<PathBuf>::new());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_blank_lines_and_surrounding_whitespace_are_ignored() {
        let envelope = load(
            "# a comment\n\
             \n\
             version = 1\n\
               enabled   =   true  \n\
             \t# an indented comment\n\
             allowed_kinds =  cargo_target_dir ,  homebrew_cache \n",
        )
        .expect("parses");

        assert!(envelope.is_enabled());
        assert_eq!(
            envelope.allowed_kinds(),
            &[ResourceKind::CargoTargetDir, ResourceKind::HomebrewCache]
        );
    }

    #[test]
    fn a_file_with_only_a_version_is_a_revoked_envelope() {
        assert_eq!(
            load("version = 1\n").expect("parses"),
            AutopilotEnvelope::revoked()
        );
    }

    #[test]
    fn a_missing_version_line_is_refused() {
        assert!(matches!(
            load("enabled = true\n"),
            Err(StoreError::MissingVersion)
        ));
    }

    #[test]
    fn a_future_version_is_refused_rather_than_read_under_this_builds_rules() {
        let err = load("version = 2\nenabled = true\n").expect_err("must refuse");
        assert!(matches!(err, StoreError::UnsupportedVersion { .. }));
        assert!(err.to_string().contains("version 1"));
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_skipped() {
        let err = load("version = 1\nmax_byte = 1\n").expect_err("must refuse");
        match err {
            StoreError::UnknownKey { line, ref key } => {
                assert_eq!(line, 2);
                assert_eq!(key, "max_byte");
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn a_line_that_is_not_a_key_value_pair_is_refused() {
        assert!(matches!(
            load("version = 1\nenabled\n"),
            Err(StoreError::MalformedLine { line: 2 })
        ));
    }

    #[test]
    fn a_repeated_single_valued_key_is_refused() {
        assert!(matches!(
            load("version = 1\nmax_actions = 1\nmax_actions = 2\n"),
            Err(StoreError::DuplicateKey { line: 3, .. })
        ));
    }

    #[test]
    fn preauthorize_ask_may_repeat() {
        let envelope = load(
            "version = 1\n\
             allowed_kinds = cargo_target_dir, xcode_derived_data\n\
             preauthorize_ask = cargo_target_dir:rebuild_cost_high\n\
             preauthorize_ask = xcode_derived_data:rebuild_cost_high\n",
        )
        .expect("parses");

        assert_eq!(envelope.ask_preauthorizations().len(), 2);
    }

    /// The file is order-insensitive even though `preauthorize_ask`
    /// requires its kind to be allowlisted already.
    #[test]
    fn a_preauthorization_listed_above_its_kind_still_loads() {
        let envelope = load(
            "version = 1\n\
             preauthorize_ask = cargo_target_dir:rebuild_cost_high\n\
             allowed_kinds = cargo_target_dir\n",
        )
        .expect("parses");

        assert!(envelope
            .ask_preauthorized(ResourceKind::CargoTargetDir, &[ReasonCode::RebuildCostHigh]));
    }

    #[test]
    fn enabled_accepts_exactly_true_and_false() {
        assert!(load("version = 1\nenabled = true\n")
            .expect("parses")
            .is_enabled());
        assert!(!load("version = 1\nenabled = false\n")
            .expect("parses")
            .is_enabled());
        for not_a_bool in ["yes", "1", "on", "True", "TRUE", ""] {
            assert!(
                matches!(
                    load(&format!("version = 1\nenabled = {not_a_bool}\n")),
                    Err(StoreError::InvalidValue { key: "enabled", .. })
                ),
                "{not_a_bool:?} must not turn Autopilot on"
            );
        }
    }

    /// Rule 1: the file cannot grant what the constructors refuse.
    #[test]
    fn a_hand_edited_budget_above_its_ceiling_is_refused_not_clamped() {
        for (key, over) in [
            ("max_actions", u64::from(ACTIONS_CEILING) + 1),
            ("max_bytes", BYTES_CEILING + 1),
            ("max_duration_secs", DURATION_CEILING.as_secs() + 1),
        ] {
            assert!(
                matches!(
                    load(&format!("version = 1\n{key} = {over}\n")),
                    Err(StoreError::Refused { line: 2, .. })
                ),
                "{key} = {over} must be refused, never clamped"
            );
        }
    }

    #[test]
    fn a_hand_edited_unknown_kind_cannot_be_allowlisted() {
        assert!(matches!(
            load("version = 1\nallowed_kinds = unknown\n"),
            Err(StoreError::Refused { line: 2, .. })
        ));
    }

    /// The load path must not become a second, softer way to pre-authorize
    /// a risk [`AutopilotEnvelope::preauthorize_ask`] refuses.
    #[test]
    fn a_hand_edited_preauthorization_of_a_refused_reason_is_refused() {
        for reason in [
            "evidence_incomplete",
            "evidence_stale",
            "evidence_probe_failed",
            "resource_in_active_use",
            "git_worktree_dirty",
            "owning_tool_live",
            "protected_git_internals",
            "evidence_fresh_and_complete",
        ] {
            assert!(
                matches!(
                    load(&format!(
                        "version = 1\n\
                         allowed_kinds = cargo_target_dir\n\
                         preauthorize_ask = cargo_target_dir:{reason}\n"
                    )),
                    Err(StoreError::Refused { line: 3, .. })
                ),
                "{reason} must not be pre-authorizable via the file"
            );
        }
    }

    #[test]
    fn a_preauthorization_for_a_kind_outside_the_allowlist_is_refused() {
        assert!(matches!(
            load(
                "version = 1\n\
                 allowed_kinds = cargo_target_dir\n\
                 preauthorize_ask = homebrew_cache:rebuild_cost_high\n"
            ),
            Err(StoreError::Refused { line: 3, .. })
        ));
    }

    #[test]
    fn a_malformed_preauthorization_is_refused() {
        for value in [
            "cargo_target_dir",
            "cargo_target_dir:",
            ":rebuild_cost_high",
            "cargo_target_dir:rebuild_cost",
            "cargo-target-dir:rebuild_cost_high",
            "",
        ] {
            assert!(
                matches!(
                    load(&format!(
                        "version = 1\n\
                         allowed_kinds = cargo_target_dir\n\
                         preauthorize_ask = {value}\n"
                    )),
                    Err(StoreError::InvalidValue {
                        key: "preauthorize_ask",
                        ..
                    })
                ),
                "{value:?} must not parse as a pre-authorization"
            );
        }
    }

    #[test]
    fn min_pressure_accepts_every_state_tag_and_none() {
        for state in PressureState::ALL {
            let envelope =
                load(&format!("version = 1\nmin_pressure = {}\n", state.as_str())).expect("parses");
            assert_eq!(envelope.min_pressure(), Some(state));
        }
        assert_eq!(
            load("version = 1\nmin_pressure = none\n")
                .expect("parses")
                .min_pressure(),
            None
        );
        for not_a_state in ["healthy", "Warn", "PRESSURE", ""] {
            assert!(
                matches!(
                    load(&format!("version = 1\nmin_pressure = {not_a_state}\n")),
                    Err(StoreError::InvalidValue {
                        key: "min_pressure",
                        ..
                    })
                ),
                "{not_a_state:?} must not parse as a pressure state"
            );
        }
    }

    #[test]
    fn a_non_numeric_budget_is_refused() {
        for (key, value) in [
            ("max_actions", "three"),
            ("max_actions", "-1"),
            ("max_bytes", "5GiB"),
            ("max_duration_secs", "1.5"),
        ] {
            assert!(
                matches!(
                    load(&format!("version = 1\n{key} = {value}\n")),
                    Err(StoreError::InvalidValue { .. })
                ),
                "{key} = {value} must be refused"
            );
        }
    }

    #[test]
    fn an_offending_value_is_truncated_in_the_error_message() {
        let long = "x".repeat(200);
        let err = load(&format!("version = 1\nmin_pressure = {long}\n")).expect_err("refused");
        let rendered = err.to_string();
        assert!(
            rendered.contains('…'),
            "expected truncation in {rendered:?}"
        );
        assert!(
            rendered.chars().count() < 120,
            "an error message must not echo an unbounded value: {rendered:?}"
        );
    }

    #[test]
    fn an_empty_allowlist_parses_as_no_kinds() {
        let envelope = load("version = 1\nallowed_kinds = \n").expect("parses");
        assert!(envelope.allowed_kinds().is_empty());
    }
}
