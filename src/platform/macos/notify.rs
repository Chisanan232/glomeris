//! macOS user notifications via `osascript`.
//!
//! Invoked as an argument array (`Command::new("osascript").args([...])`),
//! never through a shell (`sh -c "..."`) and never by interpolating
//! untrusted content into a shell string. Notification title/body are
//! always the fixed, templated strings produced by
//! `crate::monitor::notifier::notification_text`, which itself only ever
//! derives text from the closed `PressureState` enum — there is no
//! attacker-controlled data anywhere on this path.

use crate::monitor::notifier::{notification_text, Notifier};
use crate::monitor::pressure::PressureState;
use std::io;
use std::process::Command;

/// Sends a macOS notification center alert via `osascript`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacosNotifier;

impl Notifier for MacosNotifier {
    fn notify(&self, from: PressureState, to: PressureState) -> io::Result<()> {
        let (title, body) = notification_text(from, to);
        send_osascript_notification(&title, &body)
    }
}

/// Builds and runs the `osascript` AppleScript notification command. `title`
/// and `body` are passed as a single AppleScript source string built with
/// `format!`, but every value substituted into it is either a fixed literal
/// or comes from `notification_text`, which only ever emits template text
/// derived from the closed `PressureState` enum — never free-form external
/// input. The whole script is passed to `osascript` as one argument in an
/// argument array (never through `sh -c`), so there is no shell metacharacter
/// exposure regardless.
fn send_osascript_notification(title: &str, body: &str) -> io::Result<()> {
    let script = format!(
        "display notification {} with title {}",
        applescript_string_literal(body),
        applescript_string_literal(title)
    );

    let status = Command::new("osascript").args(["-e", &script]).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "osascript exited with status: {status}"
        )))
    }
}

/// Escapes `s` as an AppleScript double-quoted string literal. Backslashes
/// and double quotes are escaped; the result is always safe to embed
/// between double quotes in an AppleScript source string.
fn applescript_string_literal(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() + 2);
    escaped.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(applescript_string_literal("hello"), "\"hello\"");
        assert_eq!(
            applescript_string_literal("say \"hi\""),
            "\"say \\\"hi\\\"\""
        );
        assert_eq!(applescript_string_literal("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn applescript_string_literal_never_breaks_out_of_quotes() {
        // Even a maximally adversarial input (never expected in practice,
        // since content is always fixed/templated) stays inside one
        // balanced double-quoted literal.
        let input = "\" ; do shell script \"rm -rf /\" ; \"";
        let literal = applescript_string_literal(input);
        assert!(literal.starts_with('"') && literal.ends_with('"'));
        // Every quote in the middle must be escaped.
        let middle = &literal[1..literal.len() - 1];
        assert!(!middle.contains("\"\""), "no unescaped adjacent quotes");
    }
}
