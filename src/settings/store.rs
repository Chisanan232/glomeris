//! Persistence for [`RecoverySettings`] (HORO-1507).
//!
//! Deliberately the same shape as [`crate::autopilot::store`], and for the
//! same two reasons:
//!
//! 1. **Loading goes through the validating constructor.** Every value read
//!    out of the file is handed to [`RecoverySettings::with_changes`], which
//!    enforces the per-field bounds and the cross-field rule. There is
//!    deliberately **no** `Deserialize` impl for [`RecoverySettings`]: one
//!    would reconstruct the private fields directly and walk past all of it.
//!    A hand-edited file, or one written by the SwiftUI client, therefore
//!    cannot configure anything the CLI itself would refuse — it can only
//!    fail to load. This is what lets the GUI hold no validation logic of
//!    its own.
//! 2. **Unknown keys are an error, not noise.** A typo'd
//!    `notify_at_used_percnt = 90` that silently left the threshold at 75
//!    would be indistinguishable from the setting not working, and the user
//!    would conclude the feature is broken rather than that their file is.
//!
//! ## Differences from the envelope store, and why
//!
//! **A missing file loads as [`RecoverySettings::default`], not as an error
//! and not as "nothing configured".** The envelope's absence means *no
//! authority*, because an envelope exists only to grant. Settings are not a
//! grant: a user who has never opened the preferences pane still needs a
//! threshold to be notified at and a goal to recover toward, and the honest
//! answer for them is the built-in default rather than a failure. This is
//! also the whole of the upgrade story — a build that predates this file
//! wrote nothing, so every existing installation lands on defaults chosen to
//! match the behaviour it already had.
//!
//! A *malformed* file is still an error, as it is there. Falling back to
//! defaults on a broken file would silently discard a setting the user
//! believes is in effect, which for the alert threshold means silently not
//! telling them about a full disk.
//!
//! **An absent key inside an existing file falls back to that field's
//! default** rather than being a missing-key error. [`save_settings_at`]
//! always writes both, so an absent key means a hand-edit or another build,
//! and defaulting one field while honouring the other is the more useful of
//! the two readings. Note what this cannot do here: neither field grants
//! authority — the threshold decides when the user is *told*, and the goal is
//! a *default* a human still confirms — so a field reverting to its default
//! cannot widen what Glomeris is permitted to delete, which is the risk the
//! envelope store's equivalent note is about.
//!
//! ## File format
//!
//! `key = value`, one per line; `#` comments and blank lines ignored; every
//! key at most once.
//!
//! ```text
//! version = 1
//! notify_at_used_percent = 75
//! default_goal_used_percent = 70
//! ```

use std::io;
use std::path::{Path, PathBuf};

use super::{RecoverySettings, SettingsRejection};

/// The only file-format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// How much of an offending value an error message quotes. Same reasoning as
/// [`crate::autopilot::store`]: this file holds two numbers and never a
/// credential, but an error string can end up in a log.
const MAX_QUOTED_VALUE: usize = 40;

/// Why loading or saving settings failed.
#[derive(Debug)]
pub enum SettingsStoreError {
    Io(io::Error),
    /// The file declares a `version` this build does not understand.
    /// Refused rather than best-effort parsed, for the same reason as the
    /// envelope: a future version could make a key *mean* something
    /// different, and reading it under this build's rules would then apply a
    /// setting the user never asked for.
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
    /// A key appeared twice.
    DuplicateKey {
        line: usize,
        key: String,
    },
    /// A value that is not a number at all.
    InvalidValue {
        line: usize,
        key: &'static str,
        value: String,
    },
    /// A value that parsed as a number, but that [`RecoverySettings`]
    /// refused — out of range, non-finite, or a goal that is not below the
    /// threshold. This is rule 1 doing its job.
    ///
    /// Carries every line implicated rather than one: the cross-field
    /// rejection is a complaint about a *pair*, and naming only one of the
    /// two lines would send the user to edit the field that might well be
    /// the one they meant.
    Refused {
        lines: Vec<usize>,
        source: SettingsRejection,
    },
}

impl std::fmt::Display for SettingsStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "unsupported Glomeris settings version {found:?} \
                 (this build understands version {FORMAT_VERSION})"
            ),
            Self::MissingVersion => write!(
                f,
                "the Glomeris settings file has no `version` line \
                 (expected `version = {FORMAT_VERSION}`)"
            ),
            Self::MalformedLine { line } => write!(
                f,
                "line {line}: expected `key = value`, a comment, or a blank line"
            ),
            Self::UnknownKey { line, key } => {
                write!(f, "line {line}: unknown Glomeris settings key {key:?}")
            }
            Self::DuplicateKey { line, key } => {
                write!(f, "line {line}: {key:?} is set more than once")
            }
            Self::InvalidValue { line, key, value } => {
                write!(f, "line {line}: {value:?} is not a valid `{key}`")
            }
            Self::Refused { lines, source } => {
                write!(f, "{}: {source}", describe_lines(lines))
            }
        }
    }
}

impl std::error::Error for SettingsStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Refused { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for SettingsStoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// `line 3`, `lines 3 and 4`, or `the built-in defaults` when no line is
/// implicated — which is reachable only if a default pair ever failed
/// validation, and says so rather than printing `lines :`.
fn describe_lines(lines: &[usize]) -> String {
    match lines {
        [] => "the built-in defaults".to_string(),
        [one] => format!("line {one}"),
        [first, rest @ ..] => {
            let tail: Vec<String> = rest.iter().map(|l| l.to_string()).collect();
            format!("lines {first} and {}", tail.join(" and "))
        }
    }
}

/// Resolves the per-user settings path, using `$HOME` — same
/// `~/Library/Application Support/Glomeris/` directory and same
/// missing-`HOME` handling as
/// [`crate::autopilot::store::default_envelope_path`].
pub fn default_settings_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("settings.conf"))
}

/// Loads settings from [`default_settings_path`].
pub fn load_settings() -> Result<RecoverySettings, SettingsStoreError> {
    load_settings_at(&default_settings_path()?)
}

/// Saves `settings` to [`default_settings_path`].
pub fn save_settings(settings: &RecoverySettings) -> Result<(), SettingsStoreError> {
    save_settings_at(&default_settings_path()?, settings)
}

/// Loads settings from `path`, or [`RecoverySettings::default`] if no file is
/// there.
///
/// Read immediately before use rather than cached, so a change made in the
/// preferences pane takes effect on the next poll with no daemon restart —
/// same rule as [`crate::autopilot::store::load_envelope_at`].
pub fn load_settings_at(path: &Path) -> Result<RecoverySettings, SettingsStoreError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RecoverySettings::default()),
        Err(e) => return Err(SettingsStoreError::Io(e)),
    };
    parse_settings(&contents)
}

/// One file's worth of already-read lines, before anything is handed to
/// [`RecoverySettings`].
struct RawSettings {
    version: Option<(usize, String)>,
    notify_at_used_percent: Option<(usize, f64)>,
    default_goal_used_percent: Option<(usize, f64)>,
}

/// Parses a settings file's contents.
///
/// Two passes, for the same reason as the envelope's: the first only reads
/// lines, the second applies them. Here the second pass is a single
/// [`RecoverySettings::with_changes`] call with both fields at once, which is
/// what makes a file that lists the goal above the threshold parse
/// identically to one that lists them the other way round. Applying them in
/// file order would refuse a perfectly valid file on the strength of how the
/// user typed it.
fn parse_settings(contents: &str) -> Result<RecoverySettings, SettingsStoreError> {
    let raw = read_lines(contents)?;

    let Some((_, version)) = raw.version else {
        return Err(SettingsStoreError::MissingVersion);
    };
    if version != FORMAT_VERSION.to_string() {
        return Err(SettingsStoreError::UnsupportedVersion { found: version });
    }

    let notify_line = raw.notify_at_used_percent.map(|(line, _)| line);
    let goal_line = raw.default_goal_used_percent.map(|(line, _)| line);

    RecoverySettings::default()
        .with_changes(
            raw.notify_at_used_percent.map(|(_, value)| value),
            raw.default_goal_used_percent.map(|(_, value)| value),
        )
        .map_err(|source| SettingsStoreError::Refused {
            lines: implicated_lines(&source, notify_line, goal_line),
            source,
        })
}

/// Which lines a rejection is about. A per-field rejection names its own
/// field's line; the cross-field one names both, because it is a complaint
/// about the pair and either line could be the one to change.
fn implicated_lines(
    rejection: &SettingsRejection,
    notify_line: Option<usize>,
    goal_line: Option<usize>,
) -> Vec<usize> {
    let mut lines = match rejection {
        SettingsRejection::NotifyThresholdNotFinite
        | SettingsRejection::NotifyThresholdOutOfRange { .. } => vec![notify_line],
        SettingsRejection::GoalNotFinite
        | SettingsRejection::GoalOutOfRange { .. }
        | SettingsRejection::GoalRefused { .. } => vec![goal_line],
        SettingsRejection::GoalNotBelowNotifyThreshold { .. } => vec![notify_line, goal_line],
    };
    lines.retain(Option::is_some);
    lines.into_iter().flatten().collect()
}

fn read_lines(contents: &str) -> Result<RawSettings, SettingsStoreError> {
    let mut raw = RawSettings {
        version: None,
        notify_at_used_percent: None,
        default_goal_used_percent: None,
    };

    for (index, raw_line) in contents.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(SettingsStoreError::MalformedLine { line });
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "version" => set_once(&mut raw.version, line, key, value.to_string())?,
            "notify_at_used_percent" => set_once(
                &mut raw.notify_at_used_percent,
                line,
                key,
                parse_percent(line, "notify_at_used_percent", value)?,
            )?,
            "default_goal_used_percent" => set_once(
                &mut raw.default_goal_used_percent,
                line,
                key,
                parse_percent(line, "default_goal_used_percent", value)?,
            )?,
            _ => {
                return Err(SettingsStoreError::UnknownKey {
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
) -> Result<(), SettingsStoreError> {
    if slot.is_some() {
        return Err(SettingsStoreError::DuplicateKey {
            line,
            key: key.to_string(),
        });
    }
    *slot = Some((line, value));
    Ok(())
}

/// Parses a percentage, with a trailing `%` accepted and ignored so a file
/// written by hand as `75%` means the same as `75`.
///
/// Note what is *not* checked here: `f64::from_str` accepts `nan` and `inf`,
/// and this function passes both through on purpose. Rejecting them here
/// would duplicate a rule [`RecoverySettings::with_changes`] already owns,
/// and the two copies could then disagree — a file saying `inf` is refused
/// with the same wording a user typing `--notify-at-used-percent inf` gets,
/// because it is refused by the same code.
fn parse_percent(line: usize, key: &'static str, value: &str) -> Result<f64, SettingsStoreError> {
    let number = value.strip_suffix('%').unwrap_or(value).trim();
    number
        .parse::<f64>()
        .map_err(|_| SettingsStoreError::InvalidValue {
            line,
            key,
            value: quote_value(value),
        })
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

/// Renders `settings` in the file format above.
///
/// Both keys are always written, even when they hold the built-in default:
/// a file that omits what it agrees with cannot be told apart from one
/// written by a build that did not know the key, and a user reading the file
/// to find out what is configured should not have to know the defaults to
/// interpret it.
fn render_settings(settings: &RecoverySettings) -> String {
    let mut out = String::new();
    out.push_str("# Glomeris settings. Managed by `glomeris settings set`.\n");
    out.push_str("# Percentages are percent of capacity USED, not free.\n");
    out.push_str(&format!("version = {FORMAT_VERSION}\n"));
    out.push_str(&format!(
        "notify_at_used_percent = {}\n",
        settings.notify_at_used_percent()
    ));
    out.push_str(&format!(
        "default_goal_used_percent = {}\n",
        settings.default_goal().used_percent()
    ));
    out
}

/// Writes `settings` to `path`, creating parent directories as needed.
///
/// Writes through [`crate::atomic_write::write_atomically`] for the same
/// reason as the envelope: the GUI and the CLI both write this file, and a
/// reader that caught it half-written would see a settings file with no
/// `version` line and refuse to load at all.
pub fn save_settings_at(
    path: &Path,
    settings: &RecoverySettings,
) -> Result<(), SettingsStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_write::write_atomically(path, render_settings(settings).as_bytes())
        .map_err(SettingsStoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(contents: &str) -> Result<RecoverySettings, SettingsStoreError> {
        parse_settings(contents)
    }

    fn tag(error: &SettingsStoreError) -> &'static str {
        match error {
            SettingsStoreError::Io(_) => "io",
            SettingsStoreError::UnsupportedVersion { .. } => "unsupported_version",
            SettingsStoreError::MissingVersion => "missing_version",
            SettingsStoreError::MalformedLine { .. } => "malformed_line",
            SettingsStoreError::UnknownKey { .. } => "unknown_key",
            SettingsStoreError::DuplicateKey { .. } => "duplicate_key",
            SettingsStoreError::InvalidValue { .. } => "invalid_value",
            SettingsStoreError::Refused { .. } => "refused",
        }
    }

    #[test]
    fn a_full_file_round_trips() {
        let settings = RecoverySettings::default()
            .with_changes(Some(88.0), Some(55.0))
            .unwrap();
        let rendered = render_settings(&settings);
        assert_eq!(load(&rendered).unwrap(), settings);
    }

    #[test]
    fn the_rendered_file_says_which_axis_its_percentages_are_on() {
        let rendered = render_settings(&RecoverySettings::default());
        assert!(
            rendered.contains("percent of capacity USED, not free"),
            "{rendered}"
        );
    }

    #[test]
    fn both_keys_are_written_even_at_their_defaults() {
        let rendered = render_settings(&RecoverySettings::default());
        assert!(
            rendered.contains("notify_at_used_percent = 75\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("default_goal_used_percent = 70\n"),
            "{rendered}"
        );
    }

    /// The upgrade path. A build that predates this file wrote nothing, so
    /// every existing installation must land on defaults rather than an
    /// error.
    #[test]
    fn an_absent_file_loads_as_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-settings-absent-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("settings.conf");
        assert!(!path.exists());
        assert_eq!(
            load_settings_at(&path).unwrap(),
            RecoverySettings::default()
        );
        // Nothing was created as a side effect of reading.
        assert!(!path.exists());
    }

    #[test]
    fn saving_creates_the_parent_directory_and_reloads_identically() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-settings-save-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("nested").join("settings.conf");
        let settings = RecoverySettings::default()
            .with_changes(Some(92.5), Some(41.25))
            .unwrap();
        save_settings_at(&path, &settings).unwrap();
        assert_eq!(load_settings_at(&path).unwrap(), settings);

        // Saving twice overwrites rather than appending, so a second write
        // does not produce a duplicate-key file that then fails to load.
        save_settings_at(&path, &settings).unwrap();
        assert_eq!(load_settings_at(&path).unwrap(), settings);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let settings = load(
            "# a comment\n\nversion = 1\n\n  # indented comment\nnotify_at_used_percent = 80\n",
        )
        .unwrap();
        assert_eq!(settings.notify_at_used_percent(), 80.0);
    }

    /// The reason the second pass applies both fields at once.
    #[test]
    fn key_order_does_not_change_the_result() {
        let threshold_first =
            load("version = 1\nnotify_at_used_percent = 60\ndefault_goal_used_percent = 55\n")
                .unwrap();
        let goal_first =
            load("version = 1\ndefault_goal_used_percent = 55\nnotify_at_used_percent = 60\n")
                .unwrap();
        assert_eq!(threshold_first, goal_first);
        assert_eq!(threshold_first.notify_at_used_percent(), 60.0);
        assert_eq!(threshold_first.default_goal().used_percent(), 55.0);
    }

    #[test]
    fn an_absent_key_falls_back_to_that_fields_default() {
        let goal_only = load("version = 1\ndefault_goal_used_percent = 20\n").unwrap();
        assert_eq!(goal_only.notify_at_used_percent(), 75.0);
        assert_eq!(goal_only.default_goal().used_percent(), 20.0);

        let threshold_only = load("version = 1\nnotify_at_used_percent = 95\n").unwrap();
        assert_eq!(threshold_only.notify_at_used_percent(), 95.0);
        assert_eq!(threshold_only.default_goal().used_percent(), 70.0);

        let neither = load("version = 1\n").unwrap();
        assert_eq!(neither, RecoverySettings::default());
    }

    #[test]
    fn a_trailing_percent_sign_is_accepted() {
        let settings =
            load("version = 1\nnotify_at_used_percent = 80%\ndefault_goal_used_percent = 60 %\n")
                .unwrap();
        assert_eq!(settings.notify_at_used_percent(), 80.0);
        assert_eq!(settings.default_goal().used_percent(), 60.0);
    }

    #[test]
    fn a_missing_version_line_is_an_error() {
        let err = load("notify_at_used_percent = 80\n").unwrap_err();
        assert_eq!(tag(&err), "missing_version");
        assert!(err.to_string().contains("version = 1"), "{err}");
    }

    #[test]
    fn an_empty_file_is_an_error_rather_than_defaults() {
        // An existing-but-empty file is not the same as no file: something
        // wrote it, so reporting it is more useful than papering over it.
        assert_eq!(tag(&load("").unwrap_err()), "missing_version");
    }

    #[test]
    fn a_future_version_is_refused_rather_than_best_effort_parsed() {
        let err = load("version = 2\nnotify_at_used_percent = 80\n").unwrap_err();
        assert_eq!(tag(&err), "unsupported_version");
        assert!(err.to_string().contains("\"2\""), "{err}");
        // Not a number at all is also refused, and quoted as found.
        assert_eq!(
            tag(&load("version = latest\n").unwrap_err()),
            "unsupported_version"
        );
    }

    #[test]
    fn a_malformed_line_names_its_line_number() {
        let err = load("version = 1\nthis is not a key value pair\n").unwrap_err();
        assert_eq!(tag(&err), "malformed_line");
        assert!(err.to_string().starts_with("line 2:"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_an_error_not_a_skipped_line() {
        // The typo case from the module docs: silently ignoring this would
        // leave the user certain the feature is broken.
        let err = load("version = 1\nnotify_at_used_percnt = 90\n").unwrap_err();
        assert_eq!(tag(&err), "unknown_key");
        assert!(err.to_string().contains("notify_at_used_percnt"), "{err}");
    }

    #[test]
    fn a_duplicate_key_is_an_error() {
        let err = load("version = 1\nnotify_at_used_percent = 80\nnotify_at_used_percent = 90\n")
            .unwrap_err();
        assert_eq!(tag(&err), "duplicate_key");
        assert!(err.to_string().starts_with("line 3:"), "{err}");
    }

    #[test]
    fn a_non_numeric_value_is_an_invalid_value() {
        let err = load("version = 1\nnotify_at_used_percent = quite full\n").unwrap_err();
        assert_eq!(tag(&err), "invalid_value");
        assert!(err.to_string().contains("quite full"), "{err}");
        assert!(err.to_string().contains("notify_at_used_percent"), "{err}");
    }

    /// Rule 1: bounds are not re-checked here, so a file cannot configure
    /// anything the CLI would refuse.
    #[test]
    fn an_out_of_range_value_is_refused_by_the_settings_type_itself() {
        for (contents, token) in [
            (
                "version = 1\nnotify_at_used_percent = 120\n",
                "notify_threshold_out_of_range",
            ),
            (
                "version = 1\nnotify_at_used_percent = 0\n",
                "notify_threshold_out_of_range",
            ),
            (
                "version = 1\ndefault_goal_used_percent = 101\n",
                "goal_out_of_range",
            ),
        ] {
            let err = load(contents).unwrap_err();
            assert_eq!(tag(&err), "refused", "{contents}");
            let SettingsStoreError::Refused { source, .. } = &err else {
                unreachable!()
            };
            assert_eq!(source.as_str(), token, "{contents}");
        }
    }

    /// `f64::from_str` accepts these, and passing them through to the
    /// validating constructor is deliberate — see [`parse_percent`].
    #[test]
    fn nan_and_infinity_reach_the_validating_constructor() {
        for literal in ["nan", "NaN", "inf", "-inf", "infinity"] {
            let err = load(&format!(
                "version = 1\nnotify_at_used_percent = {literal}\n"
            ))
            .unwrap_err();
            assert_eq!(tag(&err), "refused", "{literal}");
            let SettingsStoreError::Refused { source, .. } = &err else {
                unreachable!()
            };
            assert!(
                matches!(
                    source,
                    SettingsRejection::NotifyThresholdNotFinite
                        | SettingsRejection::NotifyThresholdOutOfRange { .. }
                ),
                "{literal} produced {source:?}"
            );
        }
    }

    #[test]
    fn a_goal_at_or_above_the_threshold_is_refused_and_names_both_lines() {
        let err =
            load("version = 1\nnotify_at_used_percent = 80\ndefault_goal_used_percent = 85\n")
                .unwrap_err();
        assert_eq!(tag(&err), "refused");
        let SettingsStoreError::Refused { lines, source } = &err else {
            unreachable!()
        };
        assert_eq!(source.as_str(), "goal_not_below_notify_threshold");
        assert_eq!(lines, &vec![2, 3]);
        assert!(err.to_string().starts_with("lines 2 and 3:"), "{err}");
    }

    /// A per-field rejection names only its own line, so the message sends
    /// the user to the field that is actually wrong.
    #[test]
    fn a_per_field_rejection_names_only_its_own_line() {
        let err = load("version = 1\n\ndefault_goal_used_percent = 500\n").unwrap_err();
        assert!(err.to_string().starts_with("line 3:"), "{err}");
    }

    /// The cross-field rejection can fire with only one line present — a
    /// file that sets just the threshold, below the default goal. The
    /// message must then name the one line there is rather than inventing a
    /// number for the field that is absent.
    #[test]
    fn a_cross_field_rejection_with_one_key_present_names_only_that_line() {
        let err = load("version = 1\nnotify_at_used_percent = 60\n").unwrap_err();
        let SettingsStoreError::Refused { lines, source } = &err else {
            unreachable!("{err}")
        };
        assert_eq!(source.as_str(), "goal_not_below_notify_threshold");
        assert_eq!(lines, &vec![2]);
        assert!(err.to_string().starts_with("line 2:"), "{err}");
    }

    #[test]
    fn describe_lines_handles_none_one_and_many() {
        assert_eq!(describe_lines(&[]), "the built-in defaults");
        assert_eq!(describe_lines(&[7]), "line 7");
        assert_eq!(describe_lines(&[2, 3]), "lines 2 and 3");
    }

    #[test]
    fn a_long_offending_value_is_truncated_in_the_message() {
        let long = "x".repeat(MAX_QUOTED_VALUE * 2);
        let err = load(&format!("version = 1\nnotify_at_used_percent = {long}\n")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains('…'), "{message}");
        assert!(!message.contains(&long), "the full value was not truncated");
    }

    #[test]
    fn the_default_path_is_next_to_the_autopilot_envelope() {
        // Same directory, different file: the two are separate concerns and
        // a single combined file would make the settings pane able to
        // rewrite an authority grant.
        let settings = default_settings_path().unwrap();
        let envelope = crate::autopilot::store::default_envelope_path().unwrap();
        assert_eq!(settings.parent(), envelope.parent());
        assert_eq!(settings.file_name().unwrap(), "settings.conf");
    }
}
