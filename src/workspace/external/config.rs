//! Opt-in configuration for the external-context providers (HORO-1546).
//!
//! # Off unless a file says otherwise
//!
//! No file means no providers, which means [`super::ExternalContextResolver`]
//! never touches git, let alone a network (AC 1). There is no default host, no
//! default site and no implicit credential anywhere in this module: a provider
//! exists because somebody wrote down that it should.
//!
//! # The file holds no credential
//!
//! It holds the *name* of an environment variable. `github_token_env =
//! GLOMERIS_GITHUB_TOKEN` says where to look; the value is read once, in
//! [`build_from_env`], and lives only inside the adapter it was read for. That
//! keeps a token out of a file that gets backed up, synced, screen-shared and
//! occasionally pasted into a bug report, and it means this module can be
//! diagnosed — printed, dumped, diffed — without a secret being in the output.
//!
//! [`build_from_env`] is the only `std::env::var` site here, and
//! [`build_from_parts`] holds the rules it applies, so those rules are testable
//! without a test mutating the environment underneath a parallel one.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::error::ExternalProviderError;
use super::github::{GitHubPullRequests, GITHUB_API_BASE, GITHUB_HOST};
use super::http::UreqReadOnlyHttp;
use super::jira::JiraTasks;
use super::provider::{ExternalContextResolver, PullRequestProvider, TaskProvider};
use super::subject::SubjectResolver;

/// Bumped only for a change a version-1 reader could not understand.
pub const FORMAT_VERSION: u32 = 1;

/// The per-request deadline used when the file does not set one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The narrowest and widest deadline accepted.
///
/// Bounded at both ends because both mistakes are real: a sub-second deadline
/// turns a working provider into a permanent timeout, and an unbounded one lets
/// a hung service hold up a status command indefinitely.
const MIN_TIMEOUT_SECONDS: u64 = 1;
const MAX_TIMEOUT_SECONDS: u64 = 30;

/// What went wrong reading the file.
#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    MissingVersion,
    UnsupportedVersion {
        found: String,
    },
    MalformedLine {
        line: usize,
    },
    UnknownKey {
        line: usize,
        key: String,
    },
    DuplicateKey {
        line: usize,
        key: String,
    },
    InvalidValue {
        line: usize,
        key: &'static str,
    },
    /// Some but not all of a provider's required keys are present. Refused
    /// rather than half-configured: a file naming a Jira site with no credential
    /// variable is somebody midway through setting it up, and starting anyway
    /// would report an authentication failure instead of the truth.
    Incomplete {
        provider: &'static str,
        missing: &'static str,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "could not read the external-context file: {e}"),
            Self::MissingVersion => write!(f, "the external-context file has no version line"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "the external-context file is version {found}, and this build reads version {FORMAT_VERSION}"
            ),
            Self::MalformedLine { line } => {
                write!(f, "line {line} is not a key = value pair")
            }
            Self::UnknownKey { line, key } => write!(f, "line {line} sets an unknown key {key}"),
            Self::DuplicateKey { line, key } => {
                write!(f, "line {line} sets {key} a second time")
            }
            Self::InvalidValue { line, key } => {
                write!(f, "line {line} has an unusable value for {key}")
            }
            Self::Incomplete { provider, missing } => {
                write!(f, "the {provider} provider is missing {missing}")
            }
        }
    }
}

/// Which providers are configured, and how to reach them.
///
/// Neither field holds a credential; see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExternalContextConfig {
    pub github: Option<GitHubSettings>,
    pub jira: Option<JiraSettings>,
    pub timeout: Option<Duration>,
}

impl ExternalContextConfig {
    pub fn is_enabled(&self) -> bool {
        self.github.is_some() || self.jira.is_some()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSettings {
    /// The host a matching git remote reports. Compared against a subject's own
    /// host, so this is what keeps a repository on another forge from being
    /// asked about here.
    pub host: String,
    pub api_base: String,
    pub token_env: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraSettings {
    pub base_url: String,
    pub email: String,
    pub token_env: String,
}

/// `~/Library/Application Support/Glomeris/external-context.conf`, beside
/// `settings.conf`.
pub fn default_config_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("external-context.conf"))
}

/// Loads the configuration, or returns the all-off default if no file is there.
///
/// Read at the point of use rather than cached, same rule as
/// [`crate::settings::store::load_settings_at`]: a user who turns a provider off
/// expects the next command to stop asking.
pub fn load_config_at(path: &Path) -> Result<ExternalContextConfig, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(ExternalContextConfig::default())
        }
        Err(e) => return Err(ConfigError::Io(e)),
    };
    parse_config(&contents)
}

/// Loads from [`default_config_path`].
pub fn load_config() -> Result<ExternalContextConfig, ConfigError> {
    load_config_at(&default_config_path().map_err(ConfigError::Io)?)
}

#[derive(Default)]
struct RawConfig {
    version: Option<(usize, String)>,
    github_host: Option<(usize, String)>,
    github_api_base: Option<(usize, String)>,
    github_token_env: Option<(usize, String)>,
    jira_base_url: Option<(usize, String)>,
    jira_email: Option<(usize, String)>,
    jira_token_env: Option<(usize, String)>,
    timeout_seconds: Option<(usize, u64)>,
}

fn parse_config(contents: &str) -> Result<ExternalContextConfig, ConfigError> {
    let raw = read_lines(contents)?;

    let Some((_, version)) = raw.version else {
        return Err(ConfigError::MissingVersion);
    };
    if version != FORMAT_VERSION.to_string() {
        return Err(ConfigError::UnsupportedVersion { found: version });
    }

    let github = match (raw.github_host, raw.github_api_base, raw.github_token_env) {
        (None, None, None) => None,
        (host, api_base, token_env) => {
            // The token variable is the only key with no sensible default: a
            // host and an API base can be assumed to be github.com's pairing,
            // but guessing where somebody keeps a credential is not something to
            // do on their behalf.
            let Some((_, token_env)) = token_env else {
                return Err(ConfigError::Incomplete {
                    provider: "github",
                    missing: "github_token_env",
                });
            };
            Some(GitHubSettings {
                host: host.map_or_else(|| GITHUB_HOST.to_string(), |(_, value)| value),
                api_base: api_base.map_or_else(|| GITHUB_API_BASE.to_string(), |(_, value)| value),
                token_env,
            })
        }
    };

    let jira = match (raw.jira_base_url, raw.jira_email, raw.jira_token_env) {
        (None, None, None) => None,
        (base_url, email, token_env) => {
            // No defaults at all here: there is no canonical Jira site, and a
            // half-written stanza must not become a request to somewhere.
            let Some((_, base_url)) = base_url else {
                return Err(ConfigError::Incomplete {
                    provider: "jira",
                    missing: "jira_base_url",
                });
            };
            let Some((_, email)) = email else {
                return Err(ConfigError::Incomplete {
                    provider: "jira",
                    missing: "jira_email",
                });
            };
            let Some((_, token_env)) = token_env else {
                return Err(ConfigError::Incomplete {
                    provider: "jira",
                    missing: "jira_token_env",
                });
            };
            Some(JiraSettings {
                base_url,
                email,
                token_env,
            })
        }
    };

    Ok(ExternalContextConfig {
        github,
        jira,
        timeout: raw
            .timeout_seconds
            .map(|(_, seconds)| Duration::from_secs(seconds)),
    })
}

fn read_lines(contents: &str) -> Result<RawConfig, ConfigError> {
    let mut raw = RawConfig::default();

    for (index, raw_line) in contents.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(ConfigError::MalformedLine { line });
        };
        let key = key.trim();
        let value = value.trim().to_string();
        if value.is_empty() {
            return Err(ConfigError::MalformedLine { line });
        }

        match key {
            "version" => set_once(&mut raw.version, line, key, value)?,
            "github_host" => set_once(&mut raw.github_host, line, key, value)?,
            "github_api_base" => set_once(&mut raw.github_api_base, line, key, value)?,
            "github_token_env" => set_once(
                &mut raw.github_token_env,
                line,
                key,
                environment_name(line, "github_token_env", &value)?,
            )?,
            "jira_base_url" => set_once(&mut raw.jira_base_url, line, key, value)?,
            "jira_email" => set_once(&mut raw.jira_email, line, key, value)?,
            "jira_token_env" => set_once(
                &mut raw.jira_token_env,
                line,
                key,
                environment_name(line, "jira_token_env", &value)?,
            )?,
            "timeout_seconds" => set_once(
                &mut raw.timeout_seconds,
                line,
                key,
                timeout_seconds(line, &value)?,
            )?,
            _ => {
                return Err(ConfigError::UnknownKey {
                    line,
                    key: format!("\"{key}\""),
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
) -> Result<(), ConfigError> {
    if slot.is_some() {
        return Err(ConfigError::DuplicateKey {
            line,
            key: key.to_string(),
        });
    }
    *slot = Some((line, value));
    Ok(())
}

/// Checks that a value can be an environment variable name.
///
/// Shape only — whether the variable is *set* is a separate question answered in
/// [`build_from_env`], and answering it here would mean the parser's result
/// depended on the environment.
fn environment_name(line: usize, key: &'static str, value: &str) -> Result<String, ConfigError> {
    let usable = value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if usable {
        Ok(value.to_string())
    } else {
        Err(ConfigError::InvalidValue { line, key })
    }
}

fn timeout_seconds(line: usize, value: &str) -> Result<u64, ConfigError> {
    let seconds = value
        .parse::<u64>()
        .map_err(|_| ConfigError::InvalidValue {
            line,
            key: "timeout_seconds",
        })?;
    if (MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&seconds) {
        Ok(seconds)
    } else {
        Err(ConfigError::InvalidValue {
            line,
            key: "timeout_seconds",
        })
    }
}

/// The adapters a configuration produced, and why any of them is missing.
///
/// A configured provider that could not be built is *not* the same as an absent
/// one, and both end up here: the refusal is kept so a report can say "the
/// GitHub provider is configured but `GLOMERIS_GITHUB_TOKEN` is not set" rather
/// than falling silent and letting a user believe the lookup happened.
pub struct ConfiguredProviders {
    pull_requests: Option<GitHubPullRequests>,
    tasks: Option<JiraTasks>,
    pub github_refusal: Option<ExternalProviderError>,
    pub jira_refusal: Option<ExternalProviderError>,
}

impl ConfiguredProviders {
    /// Nothing configured.
    pub fn none() -> Self {
        Self {
            pull_requests: None,
            tasks: None,
            github_refusal: None,
            jira_refusal: None,
        }
    }

    pub fn github_is_ready(&self) -> bool {
        self.pull_requests.is_some()
    }

    pub fn jira_is_ready(&self) -> bool {
        self.tasks.is_some()
    }

    /// A resolver over whichever adapters were built.
    pub fn resolver<'a>(
        &'a self,
        subjects: &'a dyn SubjectResolver,
    ) -> ExternalContextResolver<'a> {
        ExternalContextResolver {
            subjects,
            pull_requests: self
                .pull_requests
                .as_ref()
                .map(|p| p as &dyn PullRequestProvider),
            tasks: self.tasks.as_ref().map(|t| t as &dyn TaskProvider),
        }
    }
}

/// Reads the credentials `config` points at and builds the adapters.
///
/// The only `std::env::var` site for credentials in this module. Each value is
/// held just long enough to move into the adapter it belongs to, and is never
/// returned, logged or formatted into an error — a missing variable is reported
/// by name, and a present one is never named alongside its value.
pub fn build_from_env(config: &ExternalContextConfig) -> ConfiguredProviders {
    build_from_parts(config, &|name| {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    })
}

/// The rules [`build_from_env`] applies, with the environment behind a closure.
///
/// Split out so they are testable without a test setting a process-wide
/// variable: such a test passes alone and fails beside a parallel one, and the
/// failure looks like a product bug.
pub fn build_from_parts(
    config: &ExternalContextConfig,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> ConfiguredProviders {
    let timeout = config.timeout();
    let mut built = ConfiguredProviders::none();

    if let Some(settings) = &config.github {
        match lookup(&settings.token_env) {
            Some(token) => {
                match GitHubPullRequests::new(
                    Box::new(UreqReadOnlyHttp::new(timeout)),
                    settings.host.clone(),
                    settings.api_base.clone(),
                    token,
                ) {
                    Ok(provider) => built.pull_requests = Some(provider),
                    Err(e) => built.github_refusal = Some(e),
                }
            }
            None => {
                built.github_refusal = Some(ExternalProviderError::InvalidConfiguration(format!(
                    "{} is not set",
                    settings.token_env
                )))
            }
        }
    }

    if let Some(settings) = &config.jira {
        match lookup(&settings.token_env) {
            Some(token) => {
                match JiraTasks::new(
                    Box::new(UreqReadOnlyHttp::new(timeout)),
                    settings.base_url.clone(),
                    settings.email.clone(),
                    token,
                ) {
                    Ok(provider) => built.tasks = Some(provider),
                    Err(e) => built.jira_refusal = Some(e),
                }
            }
            None => {
                built.jira_refusal = Some(ExternalProviderError::InvalidConfiguration(format!(
                    "{} is not set",
                    settings.token_env
                )))
            }
        }
    }

    built
}

#[cfg(test)]
mod tests {
    use super::super::provider::tests::StubSubjects;
    use super::*;

    fn parse(contents: &str) -> Result<ExternalContextConfig, ConfigError> {
        parse_config(contents)
    }

    /// The shipped state. Nothing configured means nothing asked, and a resolver
    /// built from it is disabled (AC 1).
    #[test]
    fn no_file_configures_nothing() {
        let dir =
            std::env::temp_dir().join(format!("glomeris-h1546-config-{}", std::process::id()));
        let config = load_config_at(&dir.join("absent.conf")).unwrap();

        assert_eq!(config, ExternalContextConfig::default());
        assert!(!config.is_enabled());

        let built = build_from_parts(&config, &|_| panic!("no credential should be looked up"));
        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        assert!(!built.resolver(&subjects).is_enabled());
    }

    /// A file with only a version configures nothing either — the version line
    /// is not itself an opt-in.
    #[test]
    fn a_version_alone_configures_nothing() {
        let config = parse("version = 1\n").unwrap();
        assert_eq!(config, ExternalContextConfig::default());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let config = parse("# a note\n\nversion = 1\n\n   # another\n").unwrap();
        assert!(!config.is_enabled());
    }

    /// The whole GitHub stanza, and the two defaults that exist because
    /// github.com's host and API base are a known pairing.
    #[test]
    fn a_github_stanza_needs_only_the_credential_variable() {
        let config = parse("version = 1\ngithub_token_env = GLOMERIS_GITHUB_TOKEN\n").unwrap();

        assert_eq!(
            config.github,
            Some(GitHubSettings {
                host: "github.com".into(),
                api_base: "https://api.github.com".into(),
                token_env: "GLOMERIS_GITHUB_TOKEN".into(),
            })
        );
        assert_eq!(config.jira, None);
        assert!(config.is_enabled());
    }

    /// An Enterprise Server installation names both, because the public pairing
    /// does not hold for it.
    #[test]
    fn an_enterprise_stanza_overrides_both_defaults() {
        let config = parse(
            "version = 1\n\
             github_host = ghe.example.test\n\
             github_api_base = https://ghe.example.test/api/v3\n\
             github_token_env = GHE_TOKEN\n",
        )
        .unwrap();

        let github = config.github.unwrap();
        assert_eq!(github.host, "ghe.example.test");
        assert_eq!(github.api_base, "https://ghe.example.test/api/v3");
    }

    /// A host with no credential variable is somebody midway through setting it
    /// up. Starting anyway would report an authentication failure instead of the
    /// truth.
    #[test]
    fn a_half_written_github_stanza_is_refused() {
        let error = parse("version = 1\ngithub_host = github.com\n").unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Incomplete {
                    provider: "github",
                    missing: "github_token_env"
                }
            ),
            "{error:?}"
        );
    }

    /// Jira has no canonical site, so every key is required and each missing one
    /// names itself.
    #[test]
    fn every_missing_jira_key_names_itself() {
        for (contents, missing) in [
            ("version = 1\njira_email = a@b.test\n", "jira_base_url"),
            (
                "version = 1\njira_base_url = https://x.atlassian.net\njira_token_env = T\n",
                "jira_email",
            ),
            (
                "version = 1\njira_base_url = https://x.atlassian.net\njira_email = a@b.test\n",
                "jira_token_env",
            ),
        ] {
            let error = parse(contents).unwrap_err();
            match error {
                ConfigError::Incomplete {
                    provider: "jira",
                    missing: named,
                } => assert_eq!(named, missing),
                other => panic!("{contents:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn a_full_jira_stanza_parses() {
        let config = parse(
            "version = 1\n\
             jira_base_url = https://example.atlassian.net\n\
             jira_email = someone@example.test\n\
             jira_token_env = GLOMERIS_JIRA_TOKEN\n",
        )
        .unwrap();

        assert_eq!(
            config.jira,
            Some(JiraSettings {
                base_url: "https://example.atlassian.net".into(),
                email: "someone@example.test".into(),
                token_env: "GLOMERIS_JIRA_TOKEN".into(),
            })
        );
    }

    /// The file records where a credential lives, never what it is. A test that
    /// had to write a token into a fixture file would be evidence the design had
    /// gone wrong.
    #[test]
    fn no_key_holds_a_credential() {
        let error = parse("version = 1\ngithub_token = ghp_something\n").unwrap_err();
        assert!(
            matches!(error, ConfigError::UnknownKey { line: 2, .. }),
            "{error:?}"
        );
    }

    /// A value that is not an environment variable name is refused where it is
    /// written rather than becoming a lookup that silently finds nothing.
    #[test]
    fn a_value_that_is_not_an_environment_name_is_refused() {
        for value in [
            "lowercase",
            "HAS-DASH",
            "HAS SPACE",
            "9LEADING",
            "$GLOMERIS_TOKEN",
            "ghp_an_actual_token",
        ] {
            let error = parse(&format!("version = 1\ngithub_token_env = {value}\n")).unwrap_err();
            assert!(
                matches!(
                    error,
                    ConfigError::InvalidValue {
                        line: 2,
                        key: "github_token_env"
                    }
                ),
                "{value} gave {error:?}"
            );
        }
        assert!(parse("version = 1\ngithub_token_env = _UNDERSCORE_FIRST\n").is_ok());
    }

    #[test]
    fn the_version_line_is_required_and_pinned() {
        assert!(matches!(
            parse("github_token_env = T\n").unwrap_err(),
            ConfigError::MissingVersion
        ));
        assert!(matches!(
            parse("version = 2\n").unwrap_err(),
            ConfigError::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn a_malformed_or_repeated_line_is_refused_by_number() {
        assert!(matches!(
            parse("version = 1\nthis is not a pair\n").unwrap_err(),
            ConfigError::MalformedLine { line: 2 }
        ));
        assert!(matches!(
            parse("version = 1\ngithub_host =\n").unwrap_err(),
            ConfigError::MalformedLine { line: 2 }
        ));
        assert!(matches!(
            parse("version = 1\ngithub_token_env = A\ngithub_token_env = B\n").unwrap_err(),
            ConfigError::DuplicateKey { line: 3, .. }
        ));
        assert!(matches!(
            parse("version = 1\nnot_a_key = x\n").unwrap_err(),
            ConfigError::UnknownKey { line: 2, .. }
        ));
    }

    /// Both ends of the deadline are bounded: a sub-second one turns a working
    /// provider into a permanent timeout, and an unbounded one lets a hung
    /// service hold up a status command.
    #[test]
    fn the_deadline_is_bounded_at_both_ends() {
        assert_eq!(
            parse("version = 1\ntimeout_seconds = 12\n")
                .unwrap()
                .timeout(),
            Duration::from_secs(12)
        );
        assert_eq!(parse("version = 1\n").unwrap().timeout(), DEFAULT_TIMEOUT);
        for value in ["0", "31", "-1", "5.5", "many"] {
            let error = parse(&format!("version = 1\ntimeout_seconds = {value}\n")).unwrap_err();
            assert!(
                matches!(
                    error,
                    ConfigError::InvalidValue {
                        key: "timeout_seconds",
                        ..
                    }
                ),
                "{value} gave {error:?}"
            );
        }
    }

    /// A configured provider whose credential variable is unset is *not* an
    /// absent provider. Falling silent here would let a user believe a lookup
    /// happened, which is the failure campaign section 10 names directly.
    #[test]
    fn a_configured_provider_with_no_credential_reports_why() {
        let config = parse(
            "version = 1\n\
             github_token_env = GLOMERIS_GITHUB_TOKEN\n\
             jira_base_url = https://example.atlassian.net\n\
             jira_email = a@b.test\n\
             jira_token_env = GLOMERIS_JIRA_TOKEN\n",
        )
        .unwrap();

        let built = build_from_parts(&config, &|_| None);

        assert!(!built.github_is_ready());
        assert!(!built.jira_is_ready());
        let github = built.github_refusal.as_ref().unwrap();
        assert_eq!(github.tag(), "invalid_configuration");
        // The variable is named so a user can act; its value never is.
        assert!(github.to_string().contains("GLOMERIS_GITHUB_TOKEN"));
        assert_eq!(
            built.jira_refusal.as_ref().unwrap().tag(),
            "invalid_configuration"
        );

        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        assert!(!built.resolver(&subjects).is_enabled());
    }

    /// An empty variable is treated as unset. An exported-but-blank credential
    /// is the shape a shell profile produces when a `security find-generic-password`
    /// fails, and sending it would produce an authentication failure instead of
    /// the real reason.
    #[test]
    fn an_empty_credential_counts_as_missing() {
        let config = parse("version = 1\ngithub_token_env = GLOMERIS_GITHUB_TOKEN\n").unwrap();

        let built = build_from_parts(&config, &|_| Some(String::new()));

        assert!(!built.github_is_ready());
        assert!(built.github_refusal.is_some());
    }

    /// With the credential present, the adapters exist and the resolver is
    /// enabled. Only the variable *name* appears in the test.
    #[test]
    fn a_present_credential_produces_the_adapters() {
        let config = parse(
            "version = 1\n\
             github_token_env = GLOMERIS_GITHUB_TOKEN\n\
             jira_base_url = https://example.atlassian.net\n\
             jira_email = a@b.test\n\
             jira_token_env = GLOMERIS_JIRA_TOKEN\n",
        )
        .unwrap();

        let asked = std::cell::RefCell::new(Vec::new());
        let built = build_from_parts(&config, &|name| {
            asked.borrow_mut().push(name.to_string());
            Some("a-read-only-credential".into())
        });

        assert!(built.github_is_ready());
        assert!(built.jira_is_ready());
        assert!(built.github_refusal.is_none());
        assert!(built.jira_refusal.is_none());
        assert_eq!(
            *asked.borrow(),
            vec!["GLOMERIS_GITHUB_TOKEN", "GLOMERIS_JIRA_TOKEN"]
        );

        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        assert!(built.resolver(&subjects).is_enabled());
    }

    /// A configuration the adapter itself refuses is reported as a refusal
    /// rather than as a silently absent provider.
    #[test]
    fn an_adapter_refusal_is_kept() {
        let config = parse(
            "version = 1\n\
             jira_base_url = https://example.atlassian.net\n\
             jira_email = has:a:colon@example.test\n\
             jira_token_env = T\n",
        )
        .unwrap();

        let built = build_from_parts(&config, &|_| Some("a-read-only-credential".into()));

        assert!(!built.jira_is_ready());
        assert_eq!(
            built.jira_refusal.as_ref().unwrap().tag(),
            "invalid_configuration"
        );
    }

    /// Nothing configured looks up nothing, so enabling the feature is the only
    /// thing that can cause a credential to be read at all.
    #[test]
    fn nothing_configured_reads_no_environment() {
        let built = build_from_parts(&ExternalContextConfig::default(), &|name| {
            panic!("{name} was looked up")
        });
        assert!(!built.github_is_ready());
        assert!(!built.jira_is_ready());
    }
}
