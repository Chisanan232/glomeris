//! Emergency recovery mode (HORO-953): a DEGRADED product path that must
//! run and produce a useful result even when SQLite/history/log writes
//! fail, there is no network, there is no LLM provider configured, and
//! there is no GUI. This is not `free --target` with a flag — see this
//! ticket's PR description "Known limitations" for what "useful result"
//! honestly means today.
//!
//! Canonical safety invariant (unchanged here): AI can recommend. Policy
//! decides. Executor verifies. Filesystem reality wins. Emergency
//! pressure is NOT permission to weaken that invariant — this module
//! reuses [`crate::policy::classify`]/[`crate::policy::authorize`] and
//! [`crate::executor::execute`] exactly as-is, and never references
//! anything network- or LLM-related. There is nothing in this file's
//! production code (the part above its own test module) for the
//! `module_never_references_network_or_llm_types` test below to find.
//!
//! ## Deliberate MVP scope decisions
//!
//! - No interactive `Ask` handling. A degraded, no-time,
//!   no-mechanism-for-consent path can only auto-execute
//!   [`crate::policy::PolicyClass::AutoSafe`] candidates; everything else
//!   (`Ask`, `Protected`) is correctly refused and counted in
//!   [`EmergencyReport::denied_candidates`], never escalated to
//!   interactive consent. This is a deliberate MVP choice, not an
//!   oversight.
//! - Bounded, cheap discovery only: candidates come from
//!   [`crate::detectors::DetectorRegistry::discover_all`] (already
//!   bounded/shallow), never from the scanner's full, still-potentially-
//!   slow filesystem walk.

use std::fs;
use std::path::Path;

/// Bound on how many error strings [`EmergencyReport`] retains — mirrors
/// [`crate::evidence::model::Evidence::push_source`]'s bounded-provenance
/// convention: this report must stay printable/bounded even in a
/// pathological run where nearly everything fails.
const MAX_ERRORS: usize = 8;

/// Result of one `glomeris emergency` run. Every field is a plain,
/// dependency-free primitive (`u32`/`u64`/`String`) so this stays
/// printable even if every other subsystem in the process has failed —
/// see the `Display` impl below. Deliberately has no `serde` impl (not
/// needed for this ticket's scope, per its own instructions).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmergencyReport {
    pub actions_attempted: u32,
    pub actions_succeeded: u32,
    pub total_bytes_freed: u64,
    /// Protected/ambiguous candidates correctly refused — i.e. every
    /// candidate whose [`crate::policy::classify`] result was not
    /// `AutoSafe`. Never incremented for an unrelated wiring gap (e.g. no
    /// registered action for an `AutoSafe` kind) — those go to `errors`
    /// instead, so this field stays a clean read of "policy said no".
    pub denied_candidates: u32,
    /// Non-fatal errors encountered along the way (e.g. a persistence
    /// write failure) — reported, never fatal. Bounded at [`MAX_ERRORS`].
    pub errors: Vec<String>,
}

impl EmergencyReport {
    /// Push a non-fatal error message, silently dropping it once `errors`
    /// has reached [`MAX_ERRORS`] — errors are advisory context for a
    /// human reading the report, not a field a caller should rely on
    /// being exhaustive.
    fn push_error(&mut self, message: impl Into<String>) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(message.into());
        }
    }
}

impl std::fmt::Display for EmergencyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "glomeris emergency report:")?;
        writeln!(f, "  actions attempted: {}", self.actions_attempted)?;
        writeln!(f, "  actions succeeded: {}", self.actions_succeeded)?;
        writeln!(f, "  bytes freed:       {}", self.total_bytes_freed)?;
        writeln!(f, "  candidates denied: {}", self.denied_candidates)?;
        if self.errors.is_empty() {
            writeln!(f, "  errors:            none")
        } else {
            writeln!(f, "  errors ({}):", self.errors.len())?;
            for err in &self.errors {
                writeln!(f, "    - {err}")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_report_has_no_errors_and_is_all_zero() {
        let report = EmergencyReport::default();
        assert_eq!(report.actions_attempted, 0);
        assert_eq!(report.actions_succeeded, 0);
        assert_eq!(report.total_bytes_freed, 0);
        assert_eq!(report.denied_candidates, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn push_error_is_bounded_at_max_errors() {
        let mut report = EmergencyReport::default();
        for i in 0..20 {
            report.push_error(format!("error-{i}"));
        }
        assert_eq!(report.errors.len(), MAX_ERRORS);
    }

    #[test]
    fn display_renders_without_panicking_for_empty_and_populated_reports() {
        let empty = EmergencyReport::default();
        assert!(empty.to_string().contains("errors:            none"));

        let mut populated = EmergencyReport {
            actions_attempted: 2,
            actions_succeeded: 1,
            total_bytes_freed: 4096,
            denied_candidates: 1,
            ..EmergencyReport::default()
        };
        populated.push_error("something non-fatal happened");
        let rendered = populated.to_string();
        assert!(rendered.contains("actions attempted: 2"));
        assert!(rendered.contains("something non-fatal happened"));
    }

    /// Structural proof that this module never references anything
    /// network- or LLM-capable: scans this file's own production source
    /// (everything before this test module starts) for a closed list of
    /// telltale names. No `actions::llm`/HORO-954 code, no HTTP client, no
    /// raw socket type, no async runtime is ever imported or named here —
    /// emergency mode is defined by NOT depending on any of it, even if
    /// such a module exists elsewhere in the crate by the time this lands.
    #[test]
    fn module_never_references_network_or_llm_types() {
        let source = include_str!("mod.rs");
        let production_code = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part");

        let forbidden_needles = [
            "reqwest",
            "hyper::",
            "TcpStream",
            "TcpListener",
            "UdpSocket",
            "tokio::net",
            "::llm",
            "llm::",
        ];
        for needle in forbidden_needles {
            assert!(
                !production_code.contains(needle),
                "emergency module's production code must never reference {needle}"
            );
        }
    }
}
