//! End-to-end proof for HORO-1309's `glomeris llm-check`: the four outcomes
//! a real BYOK setup can produce, the exit code each maps to, and the two
//! properties that make the command safe to run and safe to paste the output
//! of.
//!
//! Driven through the real binary against a loopback listener rather than
//! through `build_llm_check_report` with a fake provider — the unit tests
//! already cover the mapping. What only an end-to-end run can show is that
//! the bytes actually transmitted describe nothing about this machine, and
//! that a provider which echoes the credential it rejected cannot get it
//! back out through the report. Both are transmission properties; a test
//! holding a `Result` in memory cannot see them.
//!
//! `127.0.0.1:0` lets the kernel pick a free port, so these tests never
//! collide with a real service or a parallel copy of themselves, and no
//! traffic leaves the host.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The key the tests configure. Syntactically plausible, meaningless, and
/// never checked by the listener — nothing real is authenticated here. The
/// value matters only because several assertions look for its absence.
const FAKE_KEY: &str = "sk-not-a-real-key-loopback-only";

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

/// Reads one `Content-Length`-framed HTTP request and returns `(head, body)`.
/// Anything else is a harness bug rather than a condition to tolerate.
fn read_one_request(stream: &mut TcpStream) -> (String, String) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream for reading"));

    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read request head");
        assert!(read > 0, "connection closed mid-request-head");
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
    }

    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .expect("request must carry a parsable Content-Length");

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read request body");

    (head, String::from_utf8(body).expect("body must be UTF-8"))
}

fn write_response(stream: &mut TcpStream, status_line: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).expect("write");
    stream.flush().expect("flush");
}

/// Runs `glomeris llm-check --json` against a one-shot loopback listener that
/// answers with `status_line`/`body`, and returns the captured request
/// alongside the process output.
fn check_against(status_line: &'static str, body: &'static str) -> (String, String, Output) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let port = listener.local_addr().expect("read bound address").port();

    // Captured over a channel rather than asserted inside the thread, so a
    // failure surfaces as a test failure instead of a panic in a detached
    // thread.
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _peer) = listener.accept().expect("accept one connection");
        let captured = read_one_request(&mut stream);
        write_response(&mut stream, status_line, body);
        tx.send(captured).expect("send captured request");
    });

    let output = run_llm_check(&["--json"], &format!("http://127.0.0.1:{port}/v1"));

    let (head, request_body) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("provider request must arrive");
    server.join().expect("listener thread must not panic");

    (head, request_body, Output::from(output))
}

fn run_llm_check(args: &[&str], base_url: &str) -> std::process::Output {
    let mut command = Command::new(glomeris_bin());
    command.arg("llm-check");
    for arg in args {
        command.arg(arg);
    }
    command
        .env("GLOMERIS_LLM_BASE_URL", base_url)
        .env("GLOMERIS_LLM_API_KEY", FAKE_KEY)
        .env("GLOMERIS_LLM_MODEL", "test-model")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris llm-check")
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl From<std::process::Output> for Output {
    fn from(raw: std::process::Output) -> Self {
        Output {
            code: raw.status.code(),
            stdout: String::from_utf8_lossy(&raw.stdout).to_string(),
            stderr: String::from_utf8_lossy(&raw.stderr).to_string(),
        }
    }
}

impl Output {
    /// The `outcome` field, read out of the JSON report rather than by
    /// substring-matching the whole document, so a token appearing in an
    /// error sentence cannot be mistaken for the outcome itself.
    fn outcome(&self) -> String {
        let report: serde_json::Value = serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout must be one JSON report ({e}); stdout: {}\nstderr: {}",
                self.stdout, self.stderr
            )
        });
        report["outcome"]
            .as_str()
            .expect("outcome must be a string")
            .to_string()
    }
}

/// AC 7's sibling property for the connection test: running it must not be a
/// way to describe this machine to a provider. The request body is the two
/// fixed prompts and nothing else, so it carries no path, no `$HOME`, no
/// account name and no resource.
#[test]
fn the_transmitted_request_describes_nothing_local() {
    let completion = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
    let (head, request_body, output) = check_against("200 OK", completion);

    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
    assert_eq!(output.outcome(), "ok");

    // Non-vacuity: this really is the chat-completions request, not an empty
    // capture that would satisfy every assertion below.
    assert!(
        head.starts_with("POST /v1/chat/completions "),
        "must post to the appended endpoint: {head}"
    );
    assert!(
        request_body.contains("test-model"),
        "the configured model must be on the wire: {request_body}"
    );

    let home = std::env::var("HOME").expect("HOME must be set");
    let account = std::path::Path::new(&home)
        .file_name()
        .expect("HOME must have a last component")
        .to_string_lossy()
        .to_string();
    for forbidden in [home.as_str(), account.as_str()] {
        assert!(
            !request_body.contains(forbidden),
            "request body must not contain {forbidden}: {request_body}"
        );
    }
    // No path at all, which also covers detectors a project-root scope would
    // not bound — and proves no evidence was collected on the way here.
    let payload: serde_json::Value =
        serde_json::from_str(&request_body).expect("request body must be JSON");
    for message in payload["messages"]
        .as_array()
        .expect("messages must be an array")
    {
        let content = message["content"]
            .as_str()
            .expect("content must be a string");
        assert!(
            !content.contains('/'),
            "no prompt may contain a path separator: {content}"
        );
    }
}

/// HORO-1299's lesson, at the surface built to diagnose it: a provider that
/// answers and refuses is `rejected`, never `unreachable`, and the report
/// names the status and the path so the user can tell a bad key from a bad
/// base URL.
#[test]
fn a_provider_that_refuses_is_rejected_and_says_why() {
    let body = r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key"}}"#;
    let (_head, _request, output) = check_against("401 Unauthorized", body);

    assert_eq!(output.code, Some(1), "stderr: {}", output.stderr);
    assert_eq!(output.outcome(), "rejected");
    assert!(output.stdout.contains("HTTP 401"), "{}", output.stdout);
    assert!(
        output.stdout.contains("/v1/chat/completions"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("invalid_api_key"),
        "{}",
        output.stdout
    );
}

/// The nastiest realistic provider behaviour: echoing the rejected credential
/// back in the error body. Nothing stops a gateway doing this, so the report
/// must strip it — otherwise running a connection test is itself how the key
/// ends up in a terminal scrollback, a log and a pasted bug report.
#[test]
fn a_provider_that_echoes_the_rejected_key_cannot_leak_it_into_the_report() {
    // A `&'static str` is required by the harness, so the key is spelled out
    // here and asserted to match `FAKE_KEY` rather than interpolated.
    let body =
        r#"{"error":{"message":"Incorrect API key provided: sk-not-a-real-key-loopback-only"}}"#;
    assert!(
        body.contains(FAKE_KEY),
        "this test is only meaningful if the body really echoes the key"
    );

    let (_head, _request, output) = check_against("401 Unauthorized", body);

    assert_eq!(output.outcome(), "rejected");
    assert!(
        output.stdout.contains("<REDACTED>"),
        "the echoed key must be replaced, not merely absent by luck: {}",
        output.stdout
    );
    for stream in [&output.stdout, &output.stderr] {
        assert!(
            !stream.contains(FAKE_KEY),
            "the key must not appear in any output: {stream}"
        );
    }
}

/// A 200 that is not a usable completion is its own outcome: the endpoint and
/// credential worked, the answer did not. Collapsing this into `rejected`
/// would send a user to re-check a key that is fine.
#[test]
fn a_two_hundred_that_is_not_a_completion_is_an_unusable_response() {
    let (_head, _request, output) = check_against("200 OK", r#"{"unexpected":"shape"}"#);

    assert_eq!(output.code, Some(1), "stderr: {}", output.stderr);
    assert_eq!(output.outcome(), "unusable_response");
}

/// Nothing listening at all. Distinct from `rejected`, and the failure the
/// GUI's "test connection" button most often has to explain.
#[test]
fn nothing_listening_is_unreachable() {
    // Bind to learn a free port, then drop the listener so the port is
    // (almost certainly) closed. A race that let something else grab it would
    // make this test fail loudly, not pass falsely.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        listener.local_addr().expect("read bound address").port()
    };

    let output = Output::from(run_llm_check(
        &["--json"],
        &format!("http://127.0.0.1:{port}/v1"),
    ));

    assert_eq!(output.code, Some(1), "stderr: {}", output.stderr);
    assert_eq!(output.outcome(), "unreachable");
    // The report still says which endpoint was attempted — the one field that
    // distinguishes "wrong port" from "provider down".
    let report: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON report");
    assert_eq!(report["endpoint_path"], "/v1/chat/completions");
}

/// The base URL is configuration, not output: whatever the user typed must
/// not come back out of the report, because a host may be private
/// infrastructure. Asserted on the real end-to-end report, where a
/// regression anywhere between the provider and the printer would show up.
#[test]
fn the_report_never_echoes_the_host_only_the_path() {
    let completion = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
    let (_head, _request, output) = check_against("200 OK", completion);

    for forbidden in ["127.0.0.1", "http://", FAKE_KEY] {
        assert!(
            !output.stdout.contains(forbidden),
            "{forbidden} must not appear in the report: {}",
            output.stdout
        );
    }
    let report: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON report");
    assert_eq!(report["endpoint_path"], "/v1/chat/completions");
    assert_eq!(report["model"], "test-model");
}

/// Exit code 2 is reserved for "nothing was sent, the fix is local" — so a
/// script can tell a setup problem from a provider problem without parsing.
/// Both paths are checked because the pair is the whole point: with only one,
/// a regression that collapsed them would still pass.
#[test]
fn a_local_configuration_problem_exits_two_and_sends_nothing() {
    let missing = Output::from(
        Command::new(glomeris_bin())
            .arg("llm-check")
            .env_remove("GLOMERIS_LLM_API_KEY")
            .env_remove("GLOMERIS_LLM_BASE_URL")
            .env_remove("GLOMERIS_LLM_MODEL")
            .stdin(Stdio::null())
            .output()
            .expect("spawn glomeris llm-check"),
    );
    assert_eq!(missing.code, Some(2), "stdout: {}", missing.stdout);
    assert!(
        missing.stderr.contains("missing LLM configuration"),
        "{}",
        missing.stderr
    );

    // Set, but unusable: the user did configure all three, so telling them to
    // set the variables would be actively misleading.
    let invalid = Output::from(run_llm_check(&[], "https://gw.example/v1/chat/completions"));
    assert_eq!(invalid.code, Some(2), "stdout: {}", invalid.stdout);
    assert!(
        invalid.stderr.contains("API root"),
        "must say what to change: {}",
        invalid.stderr
    );
    assert!(
        !invalid.stderr.contains("missing LLM configuration"),
        "must not claim the variables are unset: {}",
        invalid.stderr
    );
    assert!(
        !invalid.stderr.contains(FAKE_KEY),
        "must not echo the key: {}",
        invalid.stderr
    );
}

/// A key passed in argv would be visible in `ps` and shell history, so the
/// flag is refused — and the refusal names only the flag, never its value.
#[test]
fn a_credential_flag_is_refused_without_echoing_its_value() {
    for arg in [
        "--api-key=sk-should-never-appear-anywhere",
        "--key=sk-should-never-appear-anywhere",
        "--token=sk-should-never-appear-anywhere",
    ] {
        let output = Output::from(run_llm_check(&[arg], "https://api.example/v1"));

        assert_eq!(output.code, Some(2), "stdout: {}", output.stdout);
        assert!(
            !output.stderr.contains("sk-should-never-appear-anywhere")
                && !output.stdout.contains("sk-should-never-appear-anywhere"),
            "the value must not be echoed: {}{}",
            output.stdout,
            output.stderr
        );
        assert!(
            output.stderr.contains("GLOMERIS_LLM_API_KEY"),
            "must point at the environment variable instead: {}",
            output.stderr
        );
    }
}
