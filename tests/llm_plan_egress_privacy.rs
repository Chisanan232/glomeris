//! Egress proof for HORO-1298: the bytes a live `glomeris llm-plan` run
//! actually puts on the wire contain no absolute path, no home directory
//! and no account name.
//!
//! Every other test of this property inspects a value the process still
//! holds. This one inspects the HTTP request body itself, captured by a
//! loopback listener standing in for the provider, because the defect being
//! fixed was specifically about transmission: the payload looked reasonable
//! as a data structure and was a path disclosure only once it left. A unit
//! test on `build_request_payload` cannot tell you that nothing downstream
//! of it — `OpenAiCompatibleProvider::complete`, the JSON envelope, a
//! header — adds the machine back in.
//!
//! The listener speaks just enough HTTP to read one request and answer
//! with one OpenAI-compatible completion. `127.0.0.1:0` lets the kernel
//! pick a free port, so this test never collides with a real service or
//! with a parallel copy of itself, and no traffic leaves the host.
//!
//! Non-vacuity matters as much as the assertions: a discovery run that
//! found nothing would trivially "leak nothing". The synthetic `$HOME`
//! below contains a real `Library/Developer/Xcode/DerivedData` tree, so the
//! Xcode detector is guaranteed to contribute one path-backed resource
//! whose real id embeds the marker directory name — and the test asserts
//! the payload names that resource (as `resource_1`, by kind) while the
//! marker itself never appears. The `--print-payload` control at the end
//! confirms the real path was present locally all along and was withheld
//! deliberately rather than never existing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::SystemTime;

/// Appears in the synthetic `$HOME`'s path, and therefore in the real
/// `ResourceId` of every resource discovered under it. Chosen to be
/// unmistakable and to stand in for the thing that actually leaked: the
/// OS account name in `/Users/<account>/...`.
const MARKER: &str = "glomeris-egress-marker-account";

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

/// A disposable `$HOME` containing a non-empty DerivedData tree, so the
/// Xcode detector — which is HOME-bounded and therefore reachable without
/// `--project-root` — reports exactly one resource under a path carrying
/// [`MARKER`].
fn make_marked_home() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home =
        std::env::temp_dir().join(format!("{MARKER}-{}-{}-{}", std::process::id(), nanos, n));
    let derived_data = home.join("Library/Developer/Xcode/DerivedData");
    std::fs::create_dir_all(derived_data.join("MyApp-abcdefg/Build")).expect("create DerivedData");
    std::fs::write(
        derived_data.join("MyApp-abcdefg/Build/artifact.o"),
        vec![0u8; 4096],
    )
    .expect("write DerivedData artifact");
    home
}

/// Reads one HTTP request from `stream` and returns `(head, body)`. Only
/// `Content-Length` framing is handled — that is all `ureq::send_json`
/// produces — and anything else is a test-harness bug rather than a
/// condition to tolerate silently.
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

/// Answers with an OpenAI-compatible completion whose content is an empty
/// plan: valid, so the CLI exits 0, and suggestion-free, so this test
/// stays about the request rather than about response handling (which
/// `actions::llm`'s own tests cover).
fn write_empty_plan_response(stream: &mut TcpStream) {
    let completion = r#"{"choices":[{"message":{"content":"{\"items\": []}"}}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{completion}",
        completion.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write response");
    stream.flush().expect("flush response");
}

fn run_llm_plan(home: &Path, extra_args: &[&str], env: &[(&str, String)]) -> std::process::Output {
    let mut command = Command::new(glomeris_bin());
    command.arg("llm-plan");
    for arg in extra_args {
        command.arg(arg);
    }
    command.env("HOME", home).stdin(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("failed to spawn glomeris llm-plan")
}

#[test]
fn live_request_body_contains_no_path_home_or_account_name() {
    let home = make_marked_home();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let port = listener.local_addr().expect("read bound address").port();

    // The listener runs on its own thread so the CLI's blocking request
    // has someone to talk to; the captured request comes back over a
    // channel rather than being asserted on inside the thread, so a
    // failing assertion surfaces as a test failure instead of a panic in a
    // detached thread.
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _peer) = listener.accept().expect("accept one connection");
        let captured = read_one_request(&mut stream);
        write_empty_plan_response(&mut stream);
        tx.send(captured).expect("send captured request");
    });

    let output = run_llm_plan(
        &home,
        &[],
        &[
            (
                "GLOMERIS_LLM_BASE_URL",
                format!("http://127.0.0.1:{port}/v1"),
            ),
            // A syntactically plausible but meaningless key: the listener
            // never checks it, and nothing real is authenticated here.
            (
                "GLOMERIS_LLM_API_KEY",
                "not-a-real-key-loopback-only".to_string(),
            ),
            ("GLOMERIS_LLM_MODEL", "test-model".to_string()),
        ],
    );

    let (head, body) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("provider request must arrive");
    server.join().expect("listener thread must not panic");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "llm-plan against the loopback provider must succeed (exit {:?}); stdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );

    // Non-vacuity: the marked DerivedData tree WAS discovered and IS in
    // the payload — under its wire alias. Without this, every assertion
    // below would also pass on an empty payload.
    assert!(
        body.contains("xcode_derived_data"),
        "the marked DerivedData resource must be in the payload, or this test proves nothing"
    );

    // The property itself, asserted before the wire-alias shape below so a
    // regression's first failure names the actual disclosure. On the marker
    // rather than only on `/`, and on `/` as well because that catches any
    // path from any detector, including the homebrew and docker detectors
    // that `--project-root` does not bound.
    assert!(
        !body.contains(MARKER),
        "request body must not contain the account-name marker"
    );
    assert!(
        !body.contains('/'),
        "request body must contain no path separator at all"
    );
    assert!(
        !body.contains("DerivedData") && !body.contains("Library"),
        "request body must not name a directory"
    );
    assert!(
        body.contains("resource_1"),
        "the payload must identify resources by positional wire alias"
    );
    // Headers are part of what is transmitted, so they get the same
    // treatment — a `User-Agent` or custom header carrying a path would be
    // just as much of a disclosure as the body.
    assert!(
        !head.contains(MARKER),
        "request headers must not contain the account-name marker"
    );

    // Control: the real, marker-bearing absolute path existed throughout
    // and is still available locally. Without this, the assertions above
    // would be satisfied by a build that simply failed to resolve the
    // resource's path in the first place.
    let payload = run_llm_plan(&home, &["--print-payload", "--json"], &[]);
    let payload_stdout = String::from_utf8_lossy(&payload.stdout);
    assert!(
        payload.status.success(),
        "llm-plan --print-payload must succeed; stderr: {}",
        String::from_utf8_lossy(&payload.stderr)
    );
    assert!(
        payload_stdout.contains(MARKER),
        "the local alias table must still carry the real path"
    );
    assert!(
        payload_stdout.contains("resource_aliases"),
        "the local alias table must be reported under its own field"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn print_payload_needs_no_provider_configuration() {
    // `--print-payload` is the flag an operator reaches for precisely
    // because they have not yet decided to trust a provider with this
    // machine, so requiring the three GLOMERIS_LLM_* variables — or
    // exiting 2 for their absence, as a live run does — would defeat it.
    let home = make_marked_home();

    let output = run_llm_plan(&home, &["--print-payload"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "--print-payload must not require provider configuration (exit {:?}); stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("nothing is sent by this command"),
        "expected the explicit not-sent banner, got: {stdout}"
    );
    assert!(
        stdout.contains("resource_1 = xcode_derived_data:"),
        "expected the wire alias to be shown against its real resource id, got: {stdout}"
    );

    std::fs::remove_dir_all(&home).ok();
}
