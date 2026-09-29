//! Report building for the external-context surface (HORO-1546 AC 7).
//!
//! The projection layer between [`crate::workspace::external`] and what
//! `glomeris external-context` prints, in both prose and `--json`. Same
//! division of labour as [`crate::cli::settings`]: the domain types own
//! validation and wording, this module owns shape, and `main.rs` owns exit
//! codes.
//!
//! # The list of what leaves is derived, not written
//!
//! Every vocabulary in [`egress_fields`] comes from the enum the projection
//! itself serializes — [`PullRequestState::ALL`], [`TaskState::ALL_TAGS`],
//! [`ProbeReason::ALL`]. That is the whole point of the surface. A
//! hand-maintained list beside the real one would drift on the first added
//! variant, and it would drift *quietly*, in the direction of under-stating
//! what leaves — which is the only direction that matters. A privacy preview
//! nobody checks is worse than none, because it is believed.
//!
//! The corresponding negative, [`NEVER_SENT`], cannot be derived from anything
//! — an absence has nothing to enumerate. It is pinned instead by
//! `tests/planner_model_egress_contract.rs`, which asserts the complete
//! serialized key set of a real payload, so a field that started travelling
//! would fail there rather than silently make a line here untrue.

use crate::evidence::ProbeReason;
use crate::reporting::dto::{
    ExternalContextPreviewReport, ExternalEgressFieldReport, ExternalProviderPreviewReport,
};
use crate::workspace::external::{ConfiguredProviders, ExternalContextConfig};
use crate::workspace::{ExternalSource, PullRequestState, TaskState};

/// What a configured provider is deliberately never told, and what it is never
/// asked to tell a model.
///
/// Phrased as the things a reader would reasonably assume *are* sent. A list of
/// what travels does not answer "did you send my branch name"; only naming the
/// absence does.
pub const NEVER_SENT: &[&str] = &[
    "the repository name, owner or URL",
    "the branch name",
    "the working tree's path, or any path",
    "the issue key, summary or description",
    "the pull request's number, title, body or author",
    "any account name, email address or username",
    "any credential, or any part of one",
    "any file name, file content or diff",
];

/// Project configuration and egress surface into the shape a `--json` client
/// parses.
///
/// `config_path`, `config_exists` and `config_error` are supplied by the caller
/// rather than resolved here, because this function must stay pure — the same
/// reason [`crate::cli::settings::build_settings_report`] takes its
/// `stored_at`.
///
/// `providers` is taken alongside `config` rather than derived from it, because
/// deriving it would mean reading the environment for a credential, and a
/// function that reports on a credential lookup must not be the one performing
/// it.
pub fn build_external_context_preview(
    config: &ExternalContextConfig,
    providers: &ConfiguredProviders,
    config_path: Option<String>,
    config_exists: bool,
    config_error: Option<String>,
) -> ExternalContextPreviewReport {
    let enabled = config.is_enabled();
    ExternalContextPreviewReport {
        enabled,
        config_path,
        config_exists,
        config_error,
        providers: ExternalSource::ALL
            .iter()
            .map(|source| provider_report(*source, config, providers))
            .collect(),
        // Nothing is configured, so nothing is ever asked, so nothing travels.
        // An empty list here is a stronger statement than a list of fields
        // qualified by "if you were to configure something", and it is the
        // state the product ships in.
        egress_fields: if enabled {
            egress_fields(config)
        } else {
            Vec::new()
        },
        never_sent: NEVER_SENT.to_vec(),
    }
}

fn provider_report(
    source: ExternalSource,
    config: &ExternalContextConfig,
    providers: &ConfiguredProviders,
) -> ExternalProviderPreviewReport {
    let (configured, ready, credential_env, endpoint, repository_host, refusal) = match source {
        ExternalSource::GitHubPullRequests => (
            config.github.is_some(),
            providers.github_is_ready(),
            config.github.as_ref().map(|s| s.token_env.clone()),
            config.github.as_ref().map(|s| s.api_base.clone()),
            config.github.as_ref().map(|s| s.host.clone()),
            providers.github_refusal.as_ref().map(|e| e.to_string()),
        ),
        ExternalSource::JiraIssues => (
            config.jira.is_some(),
            providers.jira_is_ready(),
            config.jira.as_ref().map(|s| s.token_env.clone()),
            config.jira.as_ref().map(|s| s.base_url.clone()),
            // Correlation is by an explicit issue key found in a branch name,
            // so there is no host to scope by — and reporting one would suggest
            // a scoping rule that does not exist.
            None,
            providers.jira_refusal.as_ref().map(|e| e.to_string()),
        ),
    };
    ExternalProviderPreviewReport {
        source: source.tag(),
        configured,
        ready,
        credential_env,
        endpoint,
        repository_host,
        refusal,
    }
}

/// Every field a configured provider's answer may contribute to a model
/// request, with the complete set of values each may carry.
///
/// One entry per serialized key of [`crate::planner::dto::ExternalFactView`],
/// per configured provider. A provider that is not configured contributes
/// nothing, because its fact never leaves the `not_attempted` it starts in and
/// [`crate::planner::project`] has nothing to report about it.
fn egress_fields(config: &ExternalContextConfig) -> Vec<ExternalEgressFieldReport> {
    let mut fields = Vec::new();
    if config.github.is_some() {
        fields.extend(fact_fields(
            ExternalSource::GitHubPullRequests,
            [
                "pull_request.source",
                "pull_request.state.status",
                "pull_request.state.value",
                "pull_request.state.unavailable_reason",
                "pull_request.observed_age_days.status",
                "pull_request.observed_age_days.value",
            ],
            PullRequestState::ALL.iter().map(|s| s.tag()).collect(),
        ));
    }
    if config.jira.is_some() {
        fields.extend(fact_fields(
            ExternalSource::JiraIssues,
            [
                "task.source",
                "task.state.status",
                "task.state.value",
                "task.state.unavailable_reason",
                "task.observed_age_days.status",
                "task.observed_age_days.value",
            ],
            TaskState::ALL_TAGS.to_vec(),
        ));
    }
    fields
}

/// The three keys one [`crate::planner::dto::ExternalFactView`] serializes,
/// expanded through the [`crate::planner::dto::Reported`] wrapper two of them
/// carry.
///
/// The key names are passed in rather than composed from a prefix, because a
/// `format!` here would produce owned strings and the report deliberately holds
/// `&'static str` — a field name that can be constructed at runtime is a field
/// name that can be constructed from data.
///
/// Listed per fact rather than once, because a reader wants to know what *their*
/// configured provider sends, and a single shared list would have to be written
/// in terms of "whichever of the two is on".
fn fact_fields(
    source: ExternalSource,
    keys: [&'static str; 6],
    states: Vec<&'static str>,
) -> Vec<ExternalEgressFieldReport> {
    let field = |field: &'static str, shape: &'static str, vocabulary: Vec<&'static str>| {
        ExternalEgressFieldReport {
            source: source.tag(),
            field,
            shape,
            vocabulary,
        }
    };
    // Four of the six keys carry a value and two carry the shape of an absence.
    // The reasons are enumerated because "unavailable" alone would hide that the
    // *kind* of failure travels: a refused credential and a quota refusal are
    // different tokens, which is §10's requirement and is also one more thing
    // leaving than a careless reading of "just the state" suggests.
    vec![
        field(keys[0], "token", vec![source.tag()]),
        field(keys[1], "status", status_vocabulary()),
        field(keys[2], "token", states),
        field(
            keys[3],
            "token",
            ProbeReason::ALL.iter().map(|r| r.tag()).collect(),
        ),
        field(keys[4], "status", status_vocabulary()),
        field(keys[5], "days", Vec::new()),
    ]
}

/// The observed/unavailable discriminator, taken from the two values
/// [`crate::planner::dto::Reported`] can actually produce rather than written
/// out, so the pair cannot drift from the wrapper.
fn status_vocabulary() -> Vec<&'static str> {
    vec![
        crate::planner::dto::Reported::<u64>::observed(0).status,
        crate::planner::dto::Reported::<u64>::unavailable(ProbeReason::Failed.tag()).status,
    ]
}

/// The prose `external-context` prints, one line per element.
///
/// Returned as lines rather than printed so the wording is testable without
/// capturing stdout — the same shape as
/// [`crate::cli::settings::describe_settings`].
pub fn describe_external_context_preview(report: &ExternalContextPreviewReport) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(error) = &report.config_error {
        // First, because everything below it is the empty default rather than
        // anything read from the file, and a reader who missed that would take
        // "nothing configured" for a fact about their setup.
        lines.push(format!("configuration refused: {error}"));
    }
    lines.push(match (&report.config_path, report.config_exists) {
        (Some(path), true) => format!("configured in:     {path}"),
        (Some(path), false) => format!("no file at:        {path}"),
        (None, _) => "configured in:     unknown (no HOME)".to_string(),
    });

    for provider in &report.providers {
        let state = if provider.ready {
            "ready".to_string()
        } else if let Some(refusal) = &provider.refusal {
            // A configured-but-unusable provider names the variable to set,
            // because "not ready" on its own sends somebody to the wrong file.
            format!("configured, not usable — {refusal}")
        } else if provider.configured {
            "configured".to_string()
        } else {
            "not configured".to_string()
        };
        lines.push(format!("{:<18} {state}", format!("{}:", provider.source)));
        if let Some(host) = &provider.repository_host {
            lines.push(format!(
                "  asked only about working trees whose remote is on {host}"
            ));
        }
    }

    if report.egress_fields.is_empty() {
        lines.push("nothing leaves this machine: no provider is configured.".to_string());
    } else {
        lines.push(format!(
            "{} fields may reach a model, and nothing else:",
            report.egress_fields.len()
        ));
        for field in &report.egress_fields {
            let values = if field.vocabulary.is_empty() {
                format!("<{}>", field.shape)
            } else {
                field.vocabulary.join(" | ")
            };
            lines.push(format!("  {}: {values}", field.field));
        }
    }

    lines.push("never sent:".to_string());
    for item in &report.never_sent {
        lines.push(format!("  {item}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::external::{GitHubSettings, JiraSettings};

    fn github() -> GitHubSettings {
        GitHubSettings {
            host: "github.com".to_string(),
            api_base: "https://api.github.com".to_string(),
            token_env: "GLOMERIS_GITHUB_TOKEN".to_string(),
        }
    }

    fn jira() -> JiraSettings {
        JiraSettings {
            base_url: "https://example.atlassian.net".to_string(),
            email: "somebody@example.com".to_string(),
            token_env: "GLOMERIS_JIRA_TOKEN".to_string(),
        }
    }

    fn preview(
        config: &ExternalContextConfig,
        credentials: &[&str],
    ) -> ExternalContextPreviewReport {
        let providers = crate::workspace::external::build_from_parts(config, &|name| {
            credentials
                .contains(&name)
                .then(|| "not-a-real-credential".to_string())
        });
        build_external_context_preview(
            config,
            &providers,
            Some("/somewhere/external-context.conf".to_string()),
            true,
            None,
        )
    }

    /// The state the product ships in: nothing configured, nothing leaves, and
    /// the report says so as a fact rather than as a hypothetical.
    #[test]
    fn nothing_configured_reports_that_nothing_leaves() {
        let report = preview(&ExternalContextConfig::default(), &[]);

        assert!(!report.enabled);
        assert!(report.egress_fields.is_empty());
        assert_eq!(report.providers.len(), 2, "both providers are still listed");
        for provider in &report.providers {
            assert!(!provider.configured, "{provider:?}");
            assert!(!provider.ready, "{provider:?}");
            assert_eq!(provider.credential_env, None);
            assert_eq!(provider.endpoint, None);
        }
        let prose = describe_external_context_preview(&report);
        assert!(
            prose
                .iter()
                .any(|line| line.contains("nothing leaves this machine")),
            "{prose:?}"
        );
    }

    /// Both providers are always listed, so a reader is never left to infer
    /// from an omission whether one was asked.
    #[test]
    fn every_provider_appears_whether_configured_or_not() {
        let config = ExternalContextConfig {
            github: Some(github()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_GITHUB_TOKEN"]);

        let sources: Vec<&str> = report.providers.iter().map(|p| p.source).collect();
        assert_eq!(sources, vec!["github_pull_requests", "jira_issues"]);
        let jira_row = &report.providers[1];
        assert!(!jira_row.configured);
        assert_eq!(jira_row.refusal, None, "an absent provider refused nothing");
    }

    /// AC 3 at the reporting layer: "not set up" and "set up and unusable" are
    /// different rows, and the unusable one names the variable to set.
    #[test]
    fn a_configured_provider_with_no_credential_is_not_an_unconfigured_one() {
        let config = ExternalContextConfig {
            github: Some(github()),
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_JIRA_TOKEN"]);

        let gh = &report.providers[0];
        assert!(gh.configured);
        assert!(!gh.ready);
        let refusal = gh.refusal.as_deref().expect("a refusal");
        assert!(refusal.contains("GLOMERIS_GITHUB_TOKEN"), "{refusal}");
        assert!(report.providers[1].ready);

        let prose = describe_external_context_preview(&report);
        assert!(
            prose
                .iter()
                .any(|line| line.contains("configured, not usable")
                    && line.contains("GLOMERIS_GITHUB_TOKEN")),
            "{prose:?}"
        );

        // And the other way round, because the two rows are built by separate
        // arms of a `match`: asserting this on one provider leaves the other
        // free to report `configured` from whether it is *ready*, which reads
        // identically in every test where the credential happens to be there.
        let swapped = preview(&config, &["GLOMERIS_GITHUB_TOKEN"]);
        let jira_row = &swapped.providers[1];
        assert!(jira_row.configured);
        assert!(!jira_row.ready);
        let refusal = jira_row.refusal.as_deref().expect("a refusal");
        assert!(refusal.contains("GLOMERIS_JIRA_TOKEN"), "{refusal}");
        assert!(swapped.providers[0].ready);
    }

    /// No credential value appears anywhere in the report, including in a
    /// refusal that names the variable it came from.
    #[test]
    fn no_credential_value_reaches_the_report() {
        let config = ExternalContextConfig {
            github: Some(github()),
            jira: Some(jira()),
            ..Default::default()
        };
        let providers = crate::workspace::external::build_from_parts(&config, &|_| {
            Some("ghp_THIS_IS_THE_SECRET".to_string())
        });
        let report = build_external_context_preview(&config, &providers, None, false, None);

        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("ghp_THIS_IS_THE_SECRET"), "{json}");
        // And the prose rendering, which is what a person pastes into a bug
        // report.
        for line in describe_external_context_preview(&report) {
            assert!(!line.contains("ghp_THIS_IS_THE_SECRET"), "{line}");
        }
    }

    /// The configured Jira account address is not echoed. It identifies a
    /// person rather than a setting anybody debugs, and this report is printed
    /// to a terminal and pasted into issues.
    #[test]
    fn no_account_address_reaches_the_report() {
        let config = ExternalContextConfig {
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_JIRA_TOKEN"]);

        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("somebody@example.com"), "{json}");
        assert!(!json.contains("\"email\""), "{json}");
        // The site itself is reported, because a person needs to see which one
        // is asked.
        assert_eq!(
            report.providers[1].endpoint.as_deref(),
            Some("https://example.atlassian.net")
        );
    }

    /// The scoping fact: only working trees on the configured host are asked
    /// about, and the report says which host that is.
    #[test]
    fn the_report_names_the_only_host_whose_repositories_are_in_scope() {
        let config = ExternalContextConfig {
            github: Some(GitHubSettings {
                host: "github.example.internal".to_string(),
                api_base: "https://github.example.internal/api/v3".to_string(),
                token_env: "GLOMERIS_GITHUB_TOKEN".to_string(),
            }),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_GITHUB_TOKEN"]);

        assert_eq!(
            report.providers[0].repository_host.as_deref(),
            Some("github.example.internal")
        );
        assert!(
            describe_external_context_preview(&report)
                .iter()
                .any(|line| line.contains("github.example.internal")),
            "the host must be visible in prose too"
        );
    }

    /// Jira has no host scope, and the report must not invent one — a
    /// `repository_host` there would describe a rule that does not exist.
    #[test]
    fn the_key_keyed_provider_claims_no_host_scope() {
        let config = ExternalContextConfig {
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_JIRA_TOKEN"]);

        assert_eq!(report.providers[1].repository_host, None);
    }

    /// AC 7. Each configured provider contributes exactly the keys its fact
    /// serializes, and each token field carries the complete vocabulary of the
    /// enum behind it.
    #[test]
    fn the_egress_list_is_the_projections_own_vocabulary() {
        let config = ExternalContextConfig {
            github: Some(github()),
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_GITHUB_TOKEN", "GLOMERIS_JIRA_TOKEN"]);

        let fields: Vec<&str> = report.egress_fields.iter().map(|f| f.field).collect();
        assert_eq!(
            fields,
            vec![
                "pull_request.source",
                "pull_request.state.status",
                "pull_request.state.value",
                "pull_request.state.unavailable_reason",
                "pull_request.observed_age_days.status",
                "pull_request.observed_age_days.value",
                "task.source",
                "task.state.status",
                "task.state.value",
                "task.state.unavailable_reason",
                "task.observed_age_days.status",
                "task.observed_age_days.value",
            ]
        );

        let vocabulary = |field: &str| -> Vec<&'static str> {
            report
                .egress_fields
                .iter()
                .find(|f| f.field == field)
                .map(|f| f.vocabulary.clone())
                .unwrap_or_else(|| panic!("{field} is missing"))
        };
        assert_eq!(
            vocabulary("pull_request.state.value"),
            PullRequestState::ALL
                .iter()
                .map(|s| s.tag())
                .collect::<Vec<_>>()
        );
        assert_eq!(vocabulary("task.state.value"), TaskState::ALL_TAGS.to_vec());
        assert_eq!(
            vocabulary("pull_request.state.unavailable_reason"),
            ProbeReason::ALL.iter().map(|r| r.tag()).collect::<Vec<_>>(),
            "the kind of failure travels too, and the preview must say so"
        );
        assert_eq!(
            vocabulary("pull_request.observed_age_days.value"),
            Vec::<&str>::new(),
            "a duration has no closed vocabulary to report"
        );
        assert_eq!(vocabulary("task.source"), vec!["jira_issues"]);
    }

    /// One provider on contributes one provider's fields, not both — otherwise
    /// the preview over-states what a half-configured setup sends, which is
    /// the same kind of untruth as under-stating.
    #[test]
    fn one_configured_provider_contributes_only_its_own_fields() {
        let config = ExternalContextConfig {
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_JIRA_TOKEN"]);

        assert!(report.enabled);
        assert!(
            report
                .egress_fields
                .iter()
                .all(|f| f.source == "jira_issues"),
            "{:?}",
            report.egress_fields
        );
        assert!(
            report
                .egress_fields
                .iter()
                .all(|f| f.field.starts_with("task.")),
            "{:?}",
            report.egress_fields
        );
    }

    /// A configured provider with no credential still sends nothing, but the
    /// egress list is about what the *configuration* permits rather than what
    /// today's environment happens to allow — a missing credential is one
    /// `export` away, and a preview that hid the fields until then would
    /// answer the wrong question.
    #[test]
    fn the_egress_list_follows_the_configuration_not_the_environment() {
        let config = ExternalContextConfig {
            github: Some(github()),
            ..Default::default()
        };
        let with = preview(&config, &["GLOMERIS_GITHUB_TOKEN"]);
        let without = preview(&config, &[]);

        assert_eq!(with.egress_fields, without.egress_fields);
        assert!(with.providers[0].ready);
        assert!(!without.providers[0].ready);
    }

    /// A refused configuration file is reported first and reports nothing
    /// configured, because a half-written stanza is refused rather than
    /// half-applied and a reader who missed that would take the empty default
    /// for a fact about their setup.
    #[test]
    fn a_refused_configuration_says_so_before_anything_else() {
        let report = build_external_context_preview(
            &ExternalContextConfig::default(),
            &ConfiguredProviders::none(),
            Some("/somewhere/external-context.conf".to_string()),
            true,
            Some("line 4: jira needs jira_token_env".to_string()),
        );

        let prose = describe_external_context_preview(&report);
        assert!(prose[0].starts_with("configuration refused:"), "{prose:?}");
        assert!(prose[0].contains("jira_token_env"), "{prose:?}");
        assert!(!report.enabled);
        assert!(report.egress_fields.is_empty());
    }

    /// The negative list is never empty and never mentions anything that is in
    /// fact sent. A line here that became untrue would be a privacy claim
    /// nothing checks.
    #[test]
    fn the_never_sent_list_contradicts_nothing_that_leaves() {
        let config = ExternalContextConfig {
            github: Some(github()),
            jira: Some(jira()),
            ..Default::default()
        };
        let report = preview(&config, &["GLOMERIS_GITHUB_TOKEN", "GLOMERIS_JIRA_TOKEN"]);

        assert!(!report.never_sent.is_empty());
        // Every token that may actually travel is a bounded state name. None of
        // them is a name, a path, a number or a URL, which is what makes the
        // list above true.
        for field in &report.egress_fields {
            for value in &field.vocabulary {
                assert!(
                    value
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                    "{value:?} in {} is not a bounded token",
                    field.field
                );
                assert!(!value.contains("http"), "{value:?}");
                assert!(!value.contains('/'), "{value:?}");
            }
        }
    }

    /// An absent path is omitted rather than serialized as null, matching every
    /// other optional field in the DTO module.
    #[test]
    fn an_unresolvable_home_omits_the_path_rather_than_nulling_it() {
        let report = build_external_context_preview(
            &ExternalContextConfig::default(),
            &ConfiguredProviders::none(),
            None,
            false,
            None,
        );

        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("config_path"), "{json}");
        assert!(json.contains("\"config_exists\":false"), "{json}");
        assert!(
            describe_external_context_preview(&report)
                .iter()
                .any(|line| line.contains("no HOME")),
            "the prose must still say why there is no path"
        );
    }
}
