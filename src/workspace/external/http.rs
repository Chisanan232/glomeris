//! The only way this module can talk to a network (HORO-1546).
//!
//! # One method, and it is a GET
//!
//! AC 2 is "the GitHub/Jira adapters perform no mutation". That could have
//! been a review promise, a comment, or a test that greps for `POST`. It is
//! instead a property of the type: an adapter holds a
//! [`ReadOnlyHttp`], `ReadOnlyHttp` has exactly one method, and that method
//! sends a GET. There is no argument an adapter could pass to reach a
//! different verb, no builder it could reconfigure, and no escape hatch to the
//! underlying client — [`UreqReadOnlyHttp`] keeps its request construction
//! private and returns an already-read body.
//!
//! `scripts/check-external-context-is-read-only.sh` additionally proves that no
//! file under this module names a mutating verb, which catches the one thing
//! the type cannot: somebody adding a second transport beside this one.
//!
//! # Why the trait exists at all
//!
//! So the adapters are testable without a network. Every interesting case —
//! a refused credential, an exhausted quota, a truncated body, a `404` on a
//! container versus a `404` on a subject — is a status and a body, and a fake
//! [`ReadOnlyHttp`] produces those deterministically. A test that needed a
//! real GitHub token to prove that `403` with no quota left is not an auth
//! failure would simply not exist.

use std::time::Duration;

use super::error::ExternalProviderError;

/// How much of a response body is read.
///
/// The body is provider-controlled text, and the adapters need a status, a
/// handful of fields and nothing else. A gateway that answers a one-issue
/// query with a megabyte does not get to decide how much memory Glomeris
/// spends on it. Generous enough that no real answer is truncated: GitHub's
/// pull-request list for one branch is a few kilobytes even fully expanded.
pub const MAX_BODY_BYTES: u64 = 1 << 20;

/// One request header.
///
/// `name` is `&'static str` because every header this module sends is a
/// literal chosen here; only the value varies, and for two of them the value
/// is a credential. Nothing about that value is ever formatted into an error
/// or a report — see [`ExternalProviderError`]'s `Display`.
pub struct HeaderPair {
    pub name: &'static str,
    pub value: String,
}

/// What one read-only GET came back as.
///
/// Three fields, and the third needs justifying: a response header is
/// generally not something to drag around. `x-ratelimit-remaining` is read
/// because GitHub reports an exhausted quota as `403` — the same status as a
/// token that lacks a scope — and the remaining count is the only thing that
/// tells them apart. Getting that wrong sends a user to rotate a credential
/// that was never the problem.
///
/// `Debug` is derived here and deliberately *not* on [`HeaderPair`]: a response
/// body is the provider's words and safe to print while diagnosing, whereas a
/// request header is where the credential is, and a derived `Debug` on it would
/// put a token one `dbg!` away from a log.
#[derive(Debug)]
pub struct HttpJson {
    pub status: u16,
    pub body: String,
    /// `x-ratelimit-remaining`, when the service sent one and it parsed as a
    /// number. `None` means the service said nothing about quota, which is not
    /// the same as saying there is quota left.
    pub rate_limit_remaining: Option<u64>,
}

/// A bounded, read-only HTTP GET.
///
/// Implementors must not follow a redirect to a different scheme, must not
/// retry a request that may have been received, and must not expose anything
/// that could build a request with another method.
pub trait ReadOnlyHttp {
    /// GETs `url` with `headers`, returning the status and body whatever the
    /// status is.
    ///
    /// A non-2xx status is a *response*, not an error: `401`, `403`, `404` and
    /// `429` each mean something specific to the adapter, and a transport that
    /// collapsed them into one opaque failure is precisely what made a BYOK
    /// `401` undiagnosable in HORO-1299. Only "no answer arrived" is an `Err`.
    fn get_json(
        &self,
        url: &str,
        headers: &[HeaderPair],
    ) -> Result<HttpJson, ExternalProviderError>;
}

/// The real transport.
///
/// Holds no credential and no URL: both arrive per call, so one instance
/// cannot accidentally carry a token from one provider into a request to
/// another.
pub struct UreqReadOnlyHttp {
    /// Wall-clock bound on one whole request, connection included.
    pub timeout: Duration,
}

impl UreqReadOnlyHttp {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl ReadOnlyHttp for UreqReadOnlyHttp {
    fn get_json(
        &self,
        url: &str,
        headers: &[HeaderPair],
    ) -> Result<HttpJson, ExternalProviderError> {
        let mut request = ureq::get(url)
            .config()
            // See the trait's note: the adapters need the status.
            .http_status_as_error(false)
            .timeout_global(Some(self.timeout))
            .build();
        for header in headers {
            request = request.header(header.name, &header.value);
        }

        let response = request.call().map_err(|e| {
            // `ureq::Error`'s `Display` reports transport and status
            // information and never request headers, so this cannot carry a
            // credential — but the mapping deliberately formats only the error
            // itself and nothing derived from `headers`, so that stays true by
            // construction rather than by trusting ureq's internals. Same
            // reasoning as `OpenAiCompatibleProvider::complete`.
            if is_timeout(&e) {
                ExternalProviderError::TimedOut
            } else {
                ExternalProviderError::Unreachable(e.to_string())
            }
        })?;

        let status = response.status().as_u16();
        // Read before `into_body()` consumes the response.
        let rate_limit_remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());

        let body = response
            .into_body()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_string()
            .map_err(|e| {
                // A failed body read must not mask the status, but there is
                // nowhere to report a status without a body here — the caller
                // gets the read failure, which names itself.
                ExternalProviderError::UnusableResponse(format!(
                    "failed to read response body: {e}"
                ))
            })?;

        Ok(HttpJson {
            status,
            body,
            rate_limit_remaining,
        })
    }
}

/// Whether a transport failure was the deadline rather than the network.
///
/// Split out so the one place that decides between two
/// [`ExternalProviderError`] variants is named and readable. The distinction
/// earns its keep because "the service did not answer in time" is a reason to
/// try again on a slow link, while "the service could not be reached" usually
/// is not — a user who cannot tell them apart will go looking for a network
/// fault that is really a two-second timeout.
fn is_timeout(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Timeout(_))
}

/// A scripted transport, so every adapter test in this module is deterministic
/// and offline.
///
/// Lives here rather than in each adapter's own test module because the
/// interesting cases are properties of *this* seam — a status, a body, a quota
/// header — and three copies of the same fake would drift.
#[cfg(test)]
pub(crate) mod fake {
    use super::{ExternalProviderError, HeaderPair, HttpJson, ReadOnlyHttp};
    use std::cell::RefCell;

    pub(crate) struct FakeHttp {
        answers: RefCell<Vec<Result<HttpJson, ExternalProviderError>>>,
        /// Every URL asked for, in order. Adapter tests assert on these: AC 5
        /// is about identifying an exact repository and branch, and the only
        /// way to prove an adapter did that is to read the URL it built.
        pub(crate) requests: RefCell<Vec<String>>,
        /// Every header *name* sent, in order, never a value. Enough to prove
        /// a credential header was attached without a test ever holding one.
        pub(crate) header_names: RefCell<Vec<&'static str>>,
    }

    impl FakeHttp {
        pub(crate) fn new(answers: Vec<Result<HttpJson, ExternalProviderError>>) -> Self {
            Self {
                answers: RefCell::new(answers),
                requests: RefCell::new(Vec::new()),
                header_names: RefCell::new(Vec::new()),
            }
        }

        /// A transport that must never be asked anything. Proves the
        /// not-configured and no-subject paths short-circuit before any
        /// request, rather than asking and discarding the answer.
        pub(crate) fn refusing() -> Self {
            Self::new(Vec::new())
        }

        pub(crate) fn urls(&self) -> Vec<String> {
            self.requests.borrow().clone()
        }
    }

    impl ReadOnlyHttp for FakeHttp {
        fn get_json(
            &self,
            url: &str,
            headers: &[HeaderPair],
        ) -> Result<HttpJson, ExternalProviderError> {
            self.requests.borrow_mut().push(url.to_string());
            self.header_names
                .borrow_mut()
                .extend(headers.iter().map(|h| h.name));
            if self.answers.borrow().is_empty() {
                return Err(ExternalProviderError::UnusableResponse(
                    "the fake transport was asked more times than it was scripted".into(),
                ));
            }
            self.answers.borrow_mut().remove(0)
        }
    }

    /// An `Ok` answer with no quota header.
    pub(crate) fn json(status: u16, body: &str) -> HttpJson {
        HttpJson {
            status,
            body: body.to_string(),
            rate_limit_remaining: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{json, FakeHttp};
    use super::*;

    #[test]
    fn the_fake_records_every_url_it_was_asked_for() {
        let http = FakeHttp::new(vec![Ok(json(200, "[]")), Ok(json(200, "{}"))]);
        assert_eq!(
            http.get_json("https://example.test/a", &[]).unwrap().status,
            200
        );
        assert_eq!(
            http.get_json("https://example.test/b", &[]).unwrap().status,
            200
        );
        assert_eq!(
            http.urls(),
            vec![
                "https://example.test/a".to_string(),
                "https://example.test/b".to_string()
            ]
        );
    }

    /// The fake records header names so an adapter test can prove a credential
    /// header was attached, and records no values so a test never holds one.
    #[test]
    fn the_fake_records_header_names_and_not_values() {
        let http = FakeHttp::new(vec![Ok(json(200, "[]"))]);
        http.get_json(
            "https://example.test/a",
            &[HeaderPair {
                name: "Authorization",
                value: "Bearer super-secret-value".into(),
            }],
        )
        .unwrap();
        assert_eq!(*http.header_names.borrow(), vec!["Authorization"]);
    }

    /// A transport scripted with nothing answers nothing, so a test asserting
    /// "no request was made" cannot pass by accident.
    #[test]
    fn a_refusing_fake_starts_with_no_answers() {
        let http = FakeHttp::refusing();
        assert!(http.urls().is_empty());
        assert!(http.get_json("https://example.test/a", &[]).is_err());
    }

    /// An over-asked fake fails rather than replaying its last answer. A test
    /// that made two lookups where the product makes one would otherwise pass
    /// while proving nothing about the second.
    #[test]
    fn the_fake_refuses_to_answer_more_often_than_it_was_scripted() {
        let http = FakeHttp::new(vec![Ok(json(200, "[]"))]);
        assert!(http.get_json("https://example.test/a", &[]).is_ok());
        let error = http.get_json("https://example.test/a", &[]).unwrap_err();
        assert_eq!(error.tag(), "unusable_response");
    }

    /// The bound exists so a provider cannot choose how much memory Glomeris
    /// spends, and it has to be big enough that no real answer is truncated.
    #[test]
    fn the_body_bound_is_generous_but_finite() {
        assert_eq!(MAX_BODY_BYTES, 1_048_576);
    }

    /// The real transport's one behavioural claim that needs no network: a
    /// URL that cannot be parsed is a failure rather than a panic, and the
    /// failure does not carry the header it was given.
    #[test]
    fn an_unusable_url_fails_without_naming_the_credential() {
        let http = UreqReadOnlyHttp::new(Duration::from_millis(250));
        let error = http
            .get_json(
                "not-a-url",
                &[HeaderPair {
                    name: "Authorization",
                    value: "Bearer super-secret-value".into(),
                }],
            )
            .unwrap_err();
        let message = error.to_string();
        assert!(
            !message.contains("super-secret-value"),
            "the credential surfaced in {message:?}"
        );
        assert!(!message.contains("Bearer"), "{message:?}");
    }
}
