//! HORO-1922: a redirect from the configured LLM endpoint must never carry the
//! bearer key or the prompt to another origin.
//!
//! Two loopback listeners stand in for "the provider" and "somewhere else".
//! The provider answers every request with a redirect to the second listener.
//! The second listener must never be contacted at all: following the redirect
//! would be the leak, and the provider's own request is the non-vacuity check
//! (it did arrive, with the Authorization header, so a redirect really was on
//! offer).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const KEY: &str = "not-a-real-key-redirect-test";

fn read_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read request head");
        assert!(n > 0, "connection closed mid-head");
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
    }
    let length = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("read body");
    head
}

fn run_against_redirect(status_line: &str) -> (String, bool, std::process::Output) {
    // "Somewhere else" records any contact and answers, so a client that wrongly
    // follows the redirect finishes and fails the assertion below instead of
    // hanging on a listener that never replies.
    let elsewhere = TcpListener::bind("127.0.0.1:0").expect("bind second listener");
    let elsewhere_port = elsewhere.local_addr().unwrap().port();
    let contacted = Arc::new(AtomicBool::new(false));
    let contacted_flag = Arc::clone(&contacted);
    elsewhere.set_nonblocking(true).expect("nonblocking");
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let watcher = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            match elsewhere.accept() {
                Ok((mut stream, _)) => {
                    contacted_flag.store(true, Ordering::Relaxed);
                    stream.set_nonblocking(false).ok();
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });

    let provider = TcpListener::bind("127.0.0.1:0").expect("bind provider listener");
    let provider_port = provider.local_addr().unwrap().port();

    let status_line = status_line.to_string();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        // Serve every connection until the test closes the channel's other side;
        // a client that followed the redirect to the provider again would show up
        // as a second head here.
        provider.set_nonblocking(true).expect("nonblocking");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut served = 0;
        while Instant::now() < deadline && served < 1 {
            match provider.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).expect("blocking stream");
                    let head = read_request(&mut stream);
                    let response = format!(
                        "HTTP/1.1 {status_line}\r\nLocation: http://127.0.0.1:{elsewhere_port}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write redirect");
                    stream.flush().expect("flush");
                    tx.send(head).expect("send head");
                    served += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("provider accept failed: {e}"),
            }
        }
    });

    let home = std::env::temp_dir().join(format!(
        "glomeris-redirect-{}-{}",
        std::process::id(),
        provider_port
    ));
    std::fs::create_dir_all(&home).expect("create home");
    let output = Command::new(env!("CARGO_BIN_EXE_glomeris"))
        .arg("llm-plan")
        .env("HOME", &home)
        .env(
            "GLOMERIS_LLM_BASE_URL",
            format!("http://127.0.0.1:{provider_port}/v1"),
        )
        .env("GLOMERIS_LLM_API_KEY", KEY)
        .env("GLOMERIS_LLM_MODEL", "test-model")
        .stdin(Stdio::null())
        .output()
        .expect("spawn glomeris llm-plan");

    let head = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the provider request must arrive");
    server.join().expect("provider thread");
    // Give a (wrongly) followed redirect a moment to land before concluding.
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    watcher.join().expect("watcher thread");
    let contacted_elsewhere = contacted.load(Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&home);
    (head, contacted_elsewhere, output)
}

#[test]
fn no_redirect_status_sends_the_key_or_prompt_to_another_origin() {
    // 302 is the class ureq follows by default (rewriting the POST to a GET);
    // 307 is the class it declines to replay. Both must end at the provider.
    for status in ["302 Found", "307 Temporary Redirect"] {
        let (head, contacted_elsewhere, output) = run_against_redirect(status);
        assert!(
            head.to_ascii_lowercase().contains("authorization: bearer"),
            "{status}: non-vacuity — the provider request carried the Authorization header: {head}"
        );
        assert!(
            !contacted_elsewhere,
            "{status}: the redirect target was contacted, so the client followed it"
        );
        assert!(
            !output.status.success(),
            "{status}: a redirect is not a completion and must not look like success"
        );
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!all.contains(KEY), "{status}: the key must never be echoed");
    }
}
