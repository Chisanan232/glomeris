//! The read-only Jira work-item adapter (HORO-1546).
//!
//! # Correlation is an exact key or nothing
//!
//! This adapter is only ever handed a [`TaskKey`], which is a private-field
//! type whose only constructor accepts `[A-Z][A-Z0-9]+-[0-9]+`. There is no
//! search endpoint here, no JQL, no title comparison and no similarity score,
//! so AC 4 holds by construction rather than by care: `fix-storage-stuff`
//! cannot become a request, because it cannot become a `TaskKey`.
//!
//! # One field, and it is a status
//!
//! The request asks for `fields=status` and the parser reads
//! `fields.status.name`. It never reads a summary, a description, a comment, an
//! assignee or a reporter — those are the contents of somebody's work tracker,
//! and this program's reason for asking is to learn one word.
//!
//! # A `404` is not an absence
//!
//! Jira returns `404` both for an issue that does not exist and for one the
//! credential may not see, and its own documentation declines to distinguish
//! them. So it cannot be reported as an observed absence: a branch naming a key
//! on another Jira site reads as *unavailable*, which is the truth, rather than
//! as "there is no task", which would be a claim this adapter cannot support.
//! Same reading as a GitHub `404` on a repository, for the same reason.
//!
//! # Read-only, structurally
//!
//! The only transport nameable here is [`ReadOnlyHttp`], whose single method is
//! a GET. Campaign section 11 requires that the *product* never mutate Jira, and
//! that is a property of the type rather than a promise in a comment (AC 2).

use serde_json::Value;

use super::error::ExternalProviderError;
use super::http::{HeaderPair, ReadOnlyHttp};
use super::provider::TaskProvider;
use super::subject::TaskKey;
use crate::workspace::TaskState;

/// A read-only Jira issue-status lookup.
pub struct JiraTasks {
    http: Box<dyn ReadOnlyHttp>,
    base_url: String,
    /// Jira Cloud authenticates an API token with the account's email address,
    /// so the pair travels together. Both are credentials for this purpose and
    /// neither is ever formatted into an error or a report — which is why this
    /// struct derives no `Debug`.
    email: String,
    token: String,
}

impl JiraTasks {
    /// Builds an adapter, or refuses.
    pub fn new(
        http: Box<dyn ReadOnlyHttp>,
        base_url: String,
        email: String,
        token: String,
    ) -> Result<Self, ExternalProviderError> {
        if base_url.is_empty() || email.is_empty() || token.is_empty() {
            return Err(ExternalProviderError::NotConfigured);
        }
        // `https` only: this request carries a credential, and a site URL is an
        // easy place for an `http://` to end up unnoticed.
        if !base_url.starts_with("https://") {
            return Err(ExternalProviderError::InvalidConfiguration(
                "the Jira base URL must start with https://".into(),
            ));
        }
        if base_url.contains('?') || base_url.contains('#') || base_url.ends_with('/') {
            return Err(ExternalProviderError::InvalidConfiguration(
                "the Jira base URL must have no query, no fragment and no trailing slash".into(),
            ));
        }
        // A colon would split the basic-auth pair somewhere other than where
        // Jira expects it, and an email containing one is not an email.
        if Self::email_is_unusable(&email) {
            return Err(ExternalProviderError::InvalidConfiguration(
                "the Jira account email must contain no colon and no whitespace".into(),
            ));
        }
        Ok(Self {
            http,
            base_url,
            email,
            token,
        })
    }

    fn email_is_unusable(email: &str) -> bool {
        email.contains(':') || email.chars().any(char::is_whitespace)
    }

    fn headers(&self) -> Vec<HeaderPair> {
        vec![
            HeaderPair {
                name: "Authorization",
                value: format!(
                    "Basic {}",
                    base64_encode(format!("{}:{}", self.email, self.token).as_bytes())
                ),
            },
            HeaderPair {
                name: "Accept",
                value: "application/json".into(),
            },
        ]
    }
}

impl TaskProvider for JiraTasks {
    fn task_state(&self, key: &TaskKey) -> Result<TaskState, ExternalProviderError> {
        // `key.as_str()` is interpolated unescaped, and that is safe because a
        // `TaskKey` cannot hold anything that escapes a path segment. The
        // guarantee lives in the type, tested in `super::subject`.
        let url = format!(
            "{}/rest/api/2/issue/{}?fields=status",
            self.base_url,
            key.as_str()
        );
        let response = self.http.get_json(&url, &self.headers())?;
        let body = match response.status {
            200 => response.body,
            401 => return Err(ExternalProviderError::AuthRejected { status: 401 }),
            403 => return Err(ExternalProviderError::AuthRejected { status: 403 }),
            429 => return Err(ExternalProviderError::RateLimited),
            // See the module note: Jira will not say whether this means the
            // issue is absent or merely invisible, so neither will Glomeris.
            status => return Err(ExternalProviderError::UnexpectedStatus { status }),
        };

        let issue: Value = serde_json::from_str(&body).map_err(|e| {
            ExternalProviderError::UnusableResponse(format!("issue was not JSON: {e}"))
        })?;
        let name = issue
            .get("fields")
            .and_then(|fields| fields.get("status"))
            .and_then(|status| status.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                // A `200` whose shape changed is a failure. Reading it as "no
                // status" would turn a provider change into a confident claim
                // about somebody's unfinished work.
                ExternalProviderError::UnusableResponse(
                    "issue JSON had no fields.status.name".to_string(),
                )
            })?;

        Ok(task_state_from_status_name(name))
    }
}

/// Maps a Jira status name onto [`TaskState`].
///
/// By name rather than by `statusCategory`, deliberately. The category is one of
/// three values, so reading it would flatten every bespoke workflow into
/// to-do/in-progress/done and make this project's own `DEV VERIFY` — a status
/// whose entire meaning is "mechanically finished, judgement outstanding" —
/// indistinguishable from `In Progress`. [`TaskState::Other`] keeps the site's
/// own word locally, and reports as `other` on the wire, so a bespoke workflow
/// survives without its vocabulary leaving the machine.
///
/// Case-insensitive because a site can rename `Done` to `DONE`, and the three
/// default statuses are worth recognising through that.
fn task_state_from_status_name(name: &str) -> TaskState {
    match name.trim().to_ascii_lowercase().as_str() {
        "to do" | "todo" | "open" | "backlog" => TaskState::ToDo,
        "in progress" | "in-progress" => TaskState::InProgress,
        "done" | "closed" | "resolved" => TaskState::Done,
        _ => TaskState::Other(name.to_string()),
    }
}

/// Standard base64, because Jira Cloud authenticates with
/// `Basic base64(email:token)`.
///
/// Written here rather than taken as a dependency: this crate has four, each
/// earning its place, and thirty lines of table-driven encoding is a smaller
/// liability than a supply-chain edge added to send one header. No padding
/// variants, no URL alphabet, no decoding — the one thing needed, tested
/// against the RFC 4648 vectors.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::http::fake::{json, FakeHttp};
    use super::*;
    use crate::workspace::external::HttpJson;

    const SITE: &str = "https://example.atlassian.net";

    fn key(value: &str) -> TaskKey {
        TaskKey::parse(value).expect("the fixture key is well formed")
    }

    fn adapter(answers: Vec<Result<HttpJson, ExternalProviderError>>) -> JiraTasks {
        JiraTasks::new(
            Box::new(FakeHttp::new(answers)),
            SITE.into(),
            "someone@example.test".into(),
            "read-only-token".into(),
        )
        .expect("the fixture configuration is valid")
    }

    fn adapter_watching(
        answers: Vec<Result<HttpJson, ExternalProviderError>>,
    ) -> (JiraTasks, std::rc::Rc<FakeHttp>) {
        let http = std::rc::Rc::new(FakeHttp::new(answers));
        let adapter = JiraTasks::new(
            Box::new(std::rc::Rc::clone(&http)),
            SITE.into(),
            "someone@example.test".into(),
            "read-only-token".into(),
        )
        .expect("the fixture configuration is valid");
        (adapter, http)
    }

    fn with_status(name: &str) -> String {
        format!(r#"{{"fields":{{"status":{{"name":"{name}"}}}}}}"#)
    }

    /// The request names the exact key, asks for one field, and asks for nothing
    /// else. The field list is part of the privacy contract: a request for the
    /// whole issue would bring back a summary and a description this program has
    /// no business holding.
    #[test]
    fn the_request_names_the_exact_key_and_asks_only_for_status() {
        let (adapter, http) = adapter_watching(vec![Ok(json(200, &with_status("In Progress")))]);

        adapter.task_state(&key("HORO-1546")).unwrap();

        assert_eq!(
            http.urls(),
            vec![
                "https://example.atlassian.net/rest/api/2/issue/HORO-1546?fields=status"
                    .to_string()
            ]
        );
    }

    #[test]
    fn the_credential_is_sent_as_a_header() {
        let (adapter, http) = adapter_watching(vec![Ok(json(200, &with_status("Done")))]);

        adapter.task_state(&key("HORO-1546")).unwrap();

        assert_eq!(*http.header_names.borrow(), vec!["Authorization", "Accept"]);
    }

    /// The three default statuses are recognised through a rename of case.
    #[test]
    fn the_default_statuses_are_recognised() {
        for (name, expected) in [
            ("To Do", TaskState::ToDo),
            ("TO DO", TaskState::ToDo),
            ("todo", TaskState::ToDo),
            ("Backlog", TaskState::ToDo),
            ("In Progress", TaskState::InProgress),
            ("in progress", TaskState::InProgress),
            ("Done", TaskState::Done),
            ("Resolved", TaskState::Done),
        ] {
            let adapter = adapter(vec![Ok(json(200, &with_status(name)))]);
            assert_eq!(
                adapter.task_state(&key("HORO-1")).unwrap(),
                expected,
                "{name}"
            );
        }
    }

    /// A bespoke status keeps the site's own word locally — this project's
    /// `DEV VERIFY` means something no three-value category can express — and
    /// reports as `other` on the wire, so the vocabulary never leaves the
    /// machine.
    #[test]
    fn a_bespoke_status_is_kept_locally_and_is_other_on_the_wire() {
        let adapter = adapter(vec![Ok(json(200, &with_status("DEV VERIFY")))]);

        let state = adapter.task_state(&key("HORO-1546")).unwrap();

        assert_eq!(state, TaskState::Other("DEV VERIFY".into()));
        assert_eq!(state.tag(), "other");
    }

    /// AC 3. Nothing a Jira site refuses becomes an observed state, and each
    /// refusal keeps its own reason.
    #[test]
    fn no_refusal_becomes_an_observed_state() {
        for (status, tag, wire) in [
            (401, "auth_rejected", "permission_denied"),
            (403, "auth_rejected", "permission_denied"),
            (429, "rate_limited", "rate_limited"),
            (500, "unexpected_status", "failed"),
            (302, "unexpected_status", "failed"),
        ] {
            let adapter = adapter(vec![Ok(json(status, "{}"))]);
            let error = adapter.task_state(&key("HORO-1")).unwrap_err();
            assert_eq!(error.tag(), tag, "{status}");
            assert_eq!(error.reason().tag(), wire, "{status}");
        }
    }

    /// Jira will not say whether a `404` means the issue is absent or merely
    /// invisible to this credential, so Glomeris does not claim either. Reading
    /// it as "no task" would be the one mistake that could make an in-progress
    /// ticket look like finished work.
    #[test]
    fn a_404_does_not_become_an_absent_task() {
        let adapter = adapter(vec![Ok(json(
            404,
            r#"{"errorMessages":["Issue Does Not Exist"]}"#,
        ))]);

        let error = adapter.task_state(&key("OTHER-1")).unwrap_err();

        assert_eq!(error.tag(), "unexpected_status");
    }

    #[test]
    fn a_transport_failure_is_passed_through() {
        let adapter = adapter(vec![Err(ExternalProviderError::Unreachable(
            "connection refused".into(),
        ))]);

        assert_eq!(
            adapter.task_state(&key("HORO-1")).unwrap_err().tag(),
            "unreachable"
        );
    }

    /// A `200` in a shape this adapter cannot read is a failure. Turning it into
    /// a state would let a provider change become a confident claim about
    /// somebody's unfinished work.
    #[test]
    fn an_unreadable_success_body_is_a_failure() {
        for body in [
            "not json",
            "{}",
            r#"{"fields":{}}"#,
            r#"{"fields":{"status":{}}}"#,
            r#"{"fields":{"status":{"name":42}}}"#,
        ] {
            let adapter = adapter(vec![Ok(json(200, body))]);
            assert_eq!(
                adapter.task_state(&key("HORO-1")).unwrap_err().tag(),
                "unusable_response",
                "{body}"
            );
        }
    }

    #[test]
    fn an_unusable_configuration_is_refused_at_construction() {
        let refused = |base: &str, email: &str, token: &str| {
            JiraTasks::new(
                Box::new(FakeHttp::refusing()),
                base.into(),
                email.into(),
                token.into(),
            )
            .err()
            .map(|e| e.tag())
        };

        assert_eq!(refused("", "a@b.test", "t"), Some("not_configured"));
        assert_eq!(refused(SITE, "", "t"), Some("not_configured"));
        assert_eq!(refused(SITE, "a@b.test", ""), Some("not_configured"));
        assert_eq!(
            refused("http://example.atlassian.net", "a@b.test", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(
            refused("https://example.atlassian.net/", "a@b.test", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(
            refused("https://example.atlassian.net#x", "a@b.test", "t"),
            Some("invalid_configuration")
        );
        // A colon would split the basic-auth pair in the wrong place.
        assert_eq!(
            refused(SITE, "a:b@example.test", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(
            refused(SITE, "a b@example.test", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(refused(SITE, "a@b.test", "t"), None);
    }

    /// The RFC 4648 section 10 vectors, plus the byte lengths that exercise each
    /// padding case. A wrong encoder here would present as an authentication
    /// failure, which is the hardest kind of bug to attribute.
    #[test]
    fn the_encoder_matches_the_published_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "{input:?}");
        }
    }

    /// The two characters that only appear at the top of the alphabet's second
    /// half, so a transposed table would be caught.
    #[test]
    fn the_encoder_reaches_the_whole_alphabet() {
        assert_eq!(base64_encode(&[0xfb, 0xf0]), "+/A=");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
        assert_eq!(base64_encode(&[0xff, 0xff, 0xff]), "////");
    }
}
