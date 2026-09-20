//
//  GlomerisVocabulary.swift
//  GlomerisMenuBar
//
//  HORO-1306: the single place where a token the Rust CLI emitted is
//  turned into wording a human can read.
//
//  Before this ticket every popover section interpolated raw CLI tokens
//  straight into its labels — "Disk: PRESSURED", "Policy:
//  UNKNOWN_INCOMPLETE", "Completeness: partial". Those are the Rust
//  enum tags, and they are correct; they are simply not English. This
//  file holds one lookup table per vocabulary the CLI emits, so the same
//  token reads the same way in every section and the wording cannot
//  drift section by section (the same reason HORO-1311 centralises CLI
//  command metadata).
//
//  ============================================================================
//  WHY THIS IS NOT POLICY IN SWIFT
//  ============================================================================
//  The standing project rule (GlomerisMenuBarApp.swift) forbids policy
//  classification in this target. This file does not classify anything.
//  It is a `String` -> wording table: it is handed a token that Rust has
//  ALREADY decided and returns display copy for it. It cannot participate
//  in a decision, and three properties of the file are what make that
//  checkable rather than merely asserted:
//
//    1. It imports Foundation only — no SwiftUI, so it cannot construct a
//       control, and no AppKit, so it cannot touch the filesystem.
//    2. It never mentions `policyLabel`/`policy_label`. Every function
//       takes an anonymous `token: String`. A `String` -> copy table does
//       not know, and cannot ask, whether the string it was handed came
//       from the policy field, the pressure field, or a test literal. That
//       decoupling is the point: the `switch`es below are switches over
//       wording, not over authority.
//    3. It contains no button, no enablement, no argument construction and
//       no reference to `executable`/`requiresConfirmation` — the two
//       fields that DO gate behaviour, and that stay where they already
//       are, in CandidateDetailView.
//
//  `scripts/check-no-policy-label-branching.sh` enforces all three
//  mechanically, as a dedicated extra check for this file. That check ADDS
//  a constraint; it does not relax the existing repo-wide one.
//
//  ============================================================================
//  THREE SEPARATE AXES — DO NOT MERGE THEM
//  ============================================================================
//  This ticket's design principles call out three concepts that must stay
//  visually and semantically distinct:
//
//    * storage impact / opportunity  — how much space is at stake
//    * policy / safety class         — what Glomeris is allowed to do
//    * evidence confidence           — how well we actually know
//
//  They are independent. A large AUTO_SAFE resource is a big opportunity,
//  not a big risk. A small PROTECTED resource is still protected. High
//  confidence in a PROTECTED classification does not make it cleanable.
//  Accordingly:
//
//    * `storageImpact(...)` returns tone `.neutral` for EVERY size, so a
//      number can never colour itself into a safety claim. There is a test
//      for this.
//    * each axis has its own `axis` string, its own symbol family and its
//      own tone scale, so two axes never render as the same chip.
//
//  ============================================================================
//  TOKEN COVERAGE, AND AN AC CORRECTION
//  ============================================================================
//  Every table below is written against the Rust source, not against the
//  ticket text, and every table has an explicit fallback for a token this
//  build has never seen (a newer CLI, or a corrupted field). The fallback
//  always reads as "unrecognised" and never as "fine" — a token we cannot
//  explain must not be presented as reassurance.
//
//  This matters concretely: HORO-1306's AC #2 asks for human wording for
//  "HEALTHY/WARN/CRITICAL", but `PressureState` (`src/monitor/pressure.rs`)
//  has FIVE variants — `HEALTHY`, `WARN`, `PRESSURED`, `CRITICAL`,
//  `EMERGENCY`. Wording only the three the AC names would have left
//  `PRESSURED` and `EMERGENCY` leaking raw tokens into the primary line of
//  the one panel whose job is to be readable at a glance, and `EMERGENCY`
//  is the most urgent state there is. All five are covered here.
//
//  Sources of truth for each table:
//    pressure        `src/monitor/pressure.rs`        PressureState::as_str
//    policy          `src/reporting/policy_label.rs`  PolicyLabel::as_str
//    completeness    `src/reporting/dto.rs`           completeness_tag
//    confidence      `src/reporting/dto.rs`           confidence_tag
//    regenerability  `src/reporting/dto.rs`           regenerability_tag
//    reasons         `src/policy/class.rs`            ReasonCode::as_str
//    kinds           `src/evidence/model.rs`          ResourceKind::tag
//    outcomes        `src/reporting/dto.rs`           ExecuteReport::outcome
//    refusals        `src/reporting/dto.rs`           ExecuteRefusalReport::reason
//    audit source    `src/monitor/persistence.rs`     AuditRecord::source
//

import Foundation

/// Visual weight for a badge. A tone is a rendering hint chosen by the
/// wording table below — never a judgment made here, and never the only
/// carrier of meaning: every badge that has a tone also renders a distinct
/// symbol and a plain-language title, so the same information survives
/// greyscale, colour-blindness and VoiceOver (this ticket's "colour is
/// never the sole indication of state").
///
/// `guarded` exists deliberately and is not a synonym for `critical`.
/// PROTECTED is not an error or a warning — it is the system working, and
/// colouring it red would teach a user that Glomeris is broken every time
/// it correctly refuses to touch their Terraform state.
enum GlomerisTone: String, Equatable, CaseIterable {
    case positive
    case neutral
    case caution
    case warning
    case critical
    case guarded
    case unknown
}

/// One CLI token and the wording this app shows for it.
///
/// `token` is kept verbatim so the raw value stays available as secondary
/// / advanced detail — a user should not need documentation to read the
/// primary meaning, but someone debugging against `--json` output or a
/// Jira ticket must still be able to see exactly what the CLI said.
struct GlomerisTerm: Equatable {
    /// The verbatim token the CLI emitted.
    let token: String
    /// Which vocabulary this token belongs to, e.g. "Safety". Used as the
    /// VoiceOver prefix so a badge reads as "Safety: Asks first" rather
    /// than a bare adjective with no subject.
    let axis: String
    /// Plain-language primary wording. Short enough for a chip.
    let title: String
    /// One sentence of explanation, shown as secondary text or a tooltip.
    let explanation: String
    /// SF Symbol name, or `nil` for vocabularies rendered as prose rather
    /// than as chips (reasons, resource kinds).
    let symbolName: String?
    let tone: GlomerisTone

    /// What VoiceOver reads for a badge. The raw token is deliberately
    /// absent: it is rendered as its own adjacent `Text`, so it is already
    /// reachable as a separate element instead of being read out in the
    /// middle of every single chip.
    var accessibilityLabel: String {
        "\(axis): \(title). \(explanation)"
    }
}

/// Lookup tables from CLI token to display wording. See the file header
/// for why a `switch` here is not policy logic.
enum GlomerisVocabulary {
    // MARK: - Disk pressure (`status --json` -> `pressure_state`)

    static let pressureAxis = "Disk space"

    /// All five `PressureState` variants, ascending in urgency. See the
    /// file header's note on HORO-1306 AC #2 naming only three of them.
    static func pressure(_ token: String) -> GlomerisTerm {
        switch token {
        case "HEALTHY":
            return term(
                token, pressureAxis, "Plenty of room",
                "Free space is comfortably above the warning threshold.",
                "checkmark.circle.fill", .positive
            )
        case "WARN":
            return term(
                token, pressureAxis, "Filling up",
                "Free space has crossed the first threshold. Worth a look, not urgent.",
                "exclamationmark.circle.fill", .caution
            )
        case "PRESSURED":
            return term(
                token, pressureAxis, "Running low",
                "Little enough free space that builds and installs may start to struggle.",
                "exclamationmark.triangle.fill", .warning
            )
        case "CRITICAL":
            return term(
                token, pressureAxis, "Critically low",
                "Very little free space left. Tools are likely to fail soon.",
                "exclamationmark.octagon.fill", .critical
            )
        case "EMERGENCY":
            return term(
                token, pressureAxis, "Out of space",
                "Effectively no free space. Writes can fail anywhere on the system.",
                "xmark.octagon.fill", .critical
            )
        default:
            return unrecognised(
                token, pressureAxis, "Unrecognised disk state",
                "The CLI reported a disk state this app has no wording for."
            )
        }
    }

    // MARK: - Safety class (`policy_label`)

    static let safetyAxis = "Safety"

    /// Wording for the four `PolicyLabel` values. Every sentence describes
    /// what Rust has already decided; none of it is a decision taken here,
    /// and nothing in this app reads these strings back to gate an action.
    static func safety(_ token: String) -> GlomerisTerm {
        switch token {
        case "AUTO_SAFE":
            return term(
                token, safetyAxis, "Safe to reclaim",
                "Evidence shows a tool can recreate this and nothing is using it, "
                    + "so Glomeris can reclaim it without asking.",
                "checkmark.shield.fill", .positive
            )
        case "ASK":
            return term(
                token, safetyAxis, "Asks first",
                "Probably fine, but not provable without you, so Glomeris will "
                    + "always ask before touching it.",
                "hand.raised.fill", .caution
            )
        case "PROTECTED":
            return term(
                token, safetyAxis, "Protected",
                "Evidence says this is not recreatable or is in use. Glomeris "
                    + "refuses to clean it, and you cannot override that here.",
                "lock.shield.fill", .guarded
            )
        case "UNKNOWN_INCOMPLETE":
            return term(
                token, safetyAxis, "Not enough evidence",
                "A measurement did not finish, went stale, or failed, so no "
                    + "safety judgement was reached. Treated as off-limits until it is.",
                "questionmark.circle.fill", .unknown
            )
        default:
            return unrecognised(
                token, safetyAxis, "Unrecognised safety class",
                "The CLI reported a safety class this app has no wording for. "
                    + "Treat it as off-limits."
            )
        }
    }

    // MARK: - Evidence completeness (`completeness`)

    static let completenessAxis = "Evidence"

    static func completeness(_ token: String) -> GlomerisTerm {
        switch token {
        case "complete":
            return term(
                token, completenessAxis, "Fully measured",
                "Every check Glomeris wanted to run finished.",
                "circle.fill", .positive
            )
        case "partial":
            return term(
                token, completenessAxis, "Partly measured",
                "Some checks finished and some did not, so the picture is incomplete.",
                "circle.lefthalf.fill", .caution
            )
        case "failed":
            return term(
                token, completenessAxis, "Measurement failed",
                "The checks Glomeris needed could not be completed at all.",
                "xmark.circle.fill", .warning
            )
        default:
            return unrecognised(
                token, completenessAxis, "Unrecognised evidence state",
                "The CLI reported a completeness value this app has no wording for."
            )
        }
    }

    // MARK: - Evidence confidence (`confidence`)

    static let confidenceAxis = "Confidence"

    static func confidence(_ token: String) -> GlomerisTerm {
        switch token {
        case "high":
            return term(
                token, confidenceAxis, "High confidence",
                "The measurements agree and are recent.",
                "star.fill", .positive
            )
        case "medium":
            return term(
                token, confidenceAxis, "Medium confidence",
                "Good enough to act on with confirmation, not good enough to assume.",
                "star.lefthalf.fill", .caution
            )
        case "low":
            return term(
                token, confidenceAxis, "Low confidence",
                "Thin or old evidence. Verify before relying on this.",
                "star", .warning
            )
        default:
            return unrecognised(
                token, confidenceAxis, "Unrecognised confidence value",
                "The CLI reported a confidence value this app has no wording for."
            )
        }
    }

    // MARK: - Regenerability (`regenerability`)

    static let regenerabilityAxis = "Recreatable"

    static func regenerability(_ token: String) -> GlomerisTerm {
        switch token {
        case "regenerable_by_tool":
            return term(
                token, regenerabilityAxis, "A tool can recreate it",
                "Cleaning this costs a download or a cache warm-up, nothing more.",
                "arrow.triangle.2.circlepath", .positive
            )
        case "regenerable_by_rebuild":
            return term(
                token, regenerabilityAxis, "A rebuild can recreate it",
                "Cleaning this costs you a full rebuild the next time you need it.",
                "hammer.fill", .caution
            )
        case "not_regenerable":
            return term(
                token, regenerabilityAxis, "Cannot be recreated",
                "Nothing can bring this back. It is never a cleanup candidate.",
                "lock.fill", .guarded
            )
        case "unknown":
            return term(
                token, regenerabilityAxis, "Unknown",
                "Glomeris cannot tell whether this could be recreated.",
                "questionmark.circle.fill", .unknown
            )
        default:
            return unrecognised(
                token, regenerabilityAxis, "Unrecognised recreatability value",
                "The CLI reported a regenerability value this app has no wording for."
            )
        }
    }

    // MARK: - Storage impact (a measured size, not a token)

    static let impactAxis = "Storage impact"

    /// Wording for a reclaimable-size estimate.
    ///
    /// The tone is `.neutral` for EVERY size, on purpose. Size is an
    /// opportunity axis, not a safety axis: a 40 GB AUTO_SAFE target
    /// directory is the best thing on the list, and a 2 MB PROTECTED
    /// credential store is still untouchable. Tinting a number red because
    /// it is large, or green because it is small, would quietly teach the
    /// opposite. `GlomerisVocabularyTests` asserts this across a wide
    /// range of sizes so no later change can reintroduce it.
    ///
    /// `isLowerBound` renders as a leading `≥` because a partially
    /// measured resource's size is a floor, not a figure — the same
    /// convention `CandidateRowViewModel` already used.
    static func storageImpact(human: String?, isLowerBound: Bool) -> GlomerisTerm {
        guard let human, !human.isEmpty else {
            return GlomerisTerm(
                token: "",
                axis: impactAxis,
                title: "Size unknown",
                explanation: "Glomeris could not measure how much space this would free.",
                symbolName: "internaldrive.fill",
                tone: .neutral
            )
        }
        let title = isLowerBound ? "\u{2265} \(human)" : human
        let explanation = isLowerBound
            ? "At least this much would be freed. Some of it could not be measured, "
                + "so the real figure is higher."
            : "This much would be freed."
        return GlomerisTerm(
            token: human,
            axis: impactAxis,
            title: title,
            explanation: explanation,
            symbolName: "internaldrive.fill",
            tone: .neutral
        )
    }

    // MARK: - Storage impact tier (`impact_tier`, HORO-1307)

    static let impactTierAxis = "Worth a look"

    /// Wording for the `impact_tier` token Rust computed for a candidate
    /// (see `reporting::impact`), or `nil` when there is nothing to call
    /// out.
    ///
    /// # Why this returns an Optional
    ///
    /// `"normal"` and `"unknown"` return `nil`, and so does a missing token
    /// from an older CLI. A chip on every single row is not emphasis, it is
    /// noise — the whole value of the tier is that only the few rows worth
    /// pausing on carry it. `"unknown"` in particular has nothing to add,
    /// because the size badge beside it already reads "Size unknown", and a
    /// second chip repeating that would spend the user's attention saying
    /// the same thing twice.
    ///
    /// An unrecognised token does NOT return `nil`: a newer CLI inventing a
    /// tier this app has no wording for should surface visibly rather than
    /// vanish, the same rule every other vocabulary here follows.
    ///
    /// # Why every tier is `.neutral`
    ///
    /// Exactly the same reason `storageImpact` is, and it matters more here
    /// because a tier is closer to looking like a verdict. "Large" is not a
    /// warning. A large AUTO_SAFE candidate is the best news the list can
    /// carry, and a normal-sized PROTECTED one is still untouchable, so
    /// hue must stay reserved for the safety axis or the two become
    /// confusable at a glance. Emphasis is carried instead by a filled
    /// badge, a distinct symbol and the word itself — which is also what
    /// keeps the distinction alive in greyscale, under a colour-vision
    /// deficiency, and for anyone reading it via VoiceOver.
    /// `GlomerisVocabularyTests` asserts no tier is ever tinted.
    static func impactTier(_ token: String?) -> GlomerisTerm? {
        switch token {
        case "large":
            return term(
                "large", impactTierAxis, "Biggest wins",
                "One of the largest things Glomeris found. Reclaiming this would "
                    + "make a real difference to your free space.",
                "arrow.up.circle.fill", .neutral
            )
        case "notable":
            return term(
                "notable", impactTierAxis, "Worth a look",
                "Big enough to be worth your attention, though not the largest thing here.",
                "arrow.up.circle", .neutral
            )
        case "normal", "unknown", nil:
            // Nothing to add. See the doc comment above.
            return nil
        case let other?:
            return unrecognised(
                other, impactTierAxis, "Unrecognised size tier",
                "The CLI reported a storage-impact tier this app has no wording for."
            )
        }
    }

    // MARK: - Background monitor (`daemon status --json`)

    static let monitorAxis = "Background monitor"

    /// Whether the launch agent is loaded. A plain boolean from the CLI,
    /// reworded — "Loaded: Yes" told the user what a plist thinks, not what
    /// the product is doing.
    static func monitorLoaded(_ loaded: Bool) -> GlomerisTerm {
        loaded
            ? GlomerisTerm(
                token: "true",
                axis: monitorAxis,
                title: "Running",
                explanation: "Glomeris is watching disk space in the background.",
                symbolName: "bolt.horizontal.circle.fill",
                tone: .positive
            )
            : GlomerisTerm(
                token: "false",
                axis: monitorAxis,
                title: "Not running",
                explanation: "Nothing is watching disk space, so you will not be warned before it runs low.",
                symbolName: "bolt.horizontal.circle",
                tone: .caution
            )
    }

    /// The last heartbeat, as an already-formatted age ("8s", "2h").
    ///
    /// Deliberately tone-neutral when a heartbeat exists, even a very old
    /// one. Deciding that some age counts as "stale" would be a freshness
    /// threshold, and thresholds are the CLI's to set — inventing one here
    /// is exactly the kind of judgment the standing rule in
    /// GlomerisMenuBarApp.swift keeps out of Swift. The age is stated
    /// plainly and the user can see for themselves.
    ///
    /// Absence is different from staleness and is not a threshold, so
    /// "never checked in" reads as unknown rather than neutral.
    static func monitorHeartbeat(ageDescription: String?) -> GlomerisTerm {
        guard let ageDescription, !ageDescription.isEmpty else {
            return GlomerisTerm(
                token: "",
                axis: monitorAxis,
                title: "No check-in yet",
                explanation: "The background monitor has never reported in.",
                symbolName: "heart.slash",
                tone: .unknown
            )
        }
        return GlomerisTerm(
            token: ageDescription,
            axis: monitorAxis,
            title: "\(ageDescription) ago",
            explanation: "The background monitor last reported in \(ageDescription) ago.",
            symbolName: "heart.fill",
            tone: .neutral
        )
    }

    // MARK: - Action outcome (`outcome`, shared by execute and the audit log)

    static let outcomeAxis = "Outcome"

    static func outcome(_ token: String) -> GlomerisTerm {
        switch token {
        case "succeeded":
            return term(
                token, outcomeAxis, "Cleaned",
                "The action ran and the space was reclaimed.",
                "checkmark.circle.fill", .positive
            )
        case "failed":
            return term(
                token, outcomeAxis, "Failed",
                "The action started and could not finish.",
                "xmark.circle.fill", .critical
            )
        case "aborted_by_revalidation":
            return term(
                token, outcomeAxis, "Stopped safely",
                "The resource changed between checking and acting, so Glomeris "
                    + "stopped before altering anything.",
                "exclamationmark.triangle.fill", .caution
            )
        case "dry_run":
            return term(
                token, outcomeAxis, "Dry run only",
                "Nothing was changed. This was a rehearsal.",
                "info.circle.fill", .neutral
            )
        default:
            return unrecognised(
                token, outcomeAxis, "Unrecognised outcome",
                "The CLI reported an outcome this app has no wording for."
            )
        }
    }

    // MARK: - Refusal reason (`ExecuteRefusalReport.reason`)

    static let refusalAxis = "Not run"

    static func refusal(_ token: String) -> GlomerisTerm {
        switch token {
        case "resource_not_found":
            return term(
                token, refusalAxis, "No longer there",
                "Nothing on disk matches this resource any more.",
                "magnifyingglass", .warning
            )
        case "action_not_found":
            return term(
                token, refusalAxis, "No action available",
                "Glomeris has no registered way to clean this kind of resource.",
                "questionmark.circle.fill", .warning
            )
        case "action_mismatch":
            return term(
                token, refusalAxis, "Action did not match",
                "The requested action is not the one this resource resolves to, "
                    + "and Glomeris will not substitute a different one.",
                "exclamationmark.triangle.fill", .warning
            )
        case "protected":
            return term(
                token, refusalAxis, "Protected",
                "This resource is protected, so the action was refused.",
                "lock.shield.fill", .guarded
            )
        case "ask_no_consent":
            return term(
                token, refusalAxis, "Needed your confirmation",
                "This resource always asks first, and no confirmation was given.",
                "hand.raised.fill", .guarded
            )
        case "ask_consent_mismatch":
            return term(
                token, refusalAxis, "Confirmation no longer valid",
                "The resource changed after you confirmed, so the confirmation "
                    + "no longer applied and Glomeris refused to reuse it.",
                "hand.raised.slash.fill", .guarded
            )
        case "auto_safe_contract_violation":
            return term(
                token, refusalAxis, "Safety contract violated",
                "The action would have done more than its automatic-safety "
                    + "contract allows, so it was refused.",
                "exclamationmark.octagon.fill", .critical
            )
        case "busy":
            return term(
                token, refusalAxis, "Another run is in progress",
                "Glomeris runs one action at a time. Try again once the other finishes.",
                "clock.fill", .neutral
            )
        default:
            return unrecognised(
                token, refusalAxis, "Refused",
                "The CLI refused for a reason this app has no wording for."
            )
        }
    }

    // MARK: - Who triggered an audited action (`AuditRecord.source`)

    static let sourceAxis = "Triggered by"

    static func actionSource(_ token: String) -> GlomerisTerm {
        switch token {
        case "execute":
            return term(
                token, sourceAxis, "You",
                "Run directly, from this app or from `glomeris execute`.",
                "person.fill", .neutral
            )
        case "free":
            return term(
                token, sourceAxis, "Automatic recovery",
                "Run by the background recovery loop as free space fell.",
                "arrow.triangle.2.circlepath", .neutral
            )
        case "emergency":
            return term(
                token, sourceAxis, "Emergency recovery",
                "Run by the emergency path, after ordinary recovery was not enough.",
                "exclamationmark.octagon.fill", .warning
            )
        default:
            return unrecognised(
                token, sourceAxis, "Unrecognised trigger",
                "The audit log recorded a trigger this app has no wording for."
            )
        }
    }

    // MARK: - Resource kind (`kind`)

    static let kindAxis = "Kind"

    /// Rendered as prose rather than as a chip, so `symbolName` is `nil`:
    /// the kind is what the row's primary line already says, and giving it
    /// its own coloured badge would compete with the three axes that
    /// actually need to be scanned.
    static func kind(_ token: String) -> GlomerisTerm {
        switch token {
        case "xcode_derived_data":
            return prose(token, kindAxis, "Xcode derived data",
                         "Xcode's per-project build intermediates and indexes.")
        case "homebrew_cache":
            return prose(token, kindAxis, "Homebrew download cache",
                         "Bottles and source archives Homebrew has already installed from.")
        case "cargo_target_dir":
            return prose(token, kindAxis, "Rust build output",
                         "A Cargo `target/` directory: compiled artifacts for one project.")
        case "cargo_registry_cache":
            return prose(token, kindAxis, "Cargo registry cache",
                         "Crate sources and archives Cargo can download again.")
        case "node_modules":
            return prose(token, kindAxis, "node_modules",
                         "Installed JavaScript dependencies for one project.")
        case "node_package_manager_cache":
            return prose(token, kindAxis, "JavaScript package cache",
                         "npm, pnpm or Yarn's shared download cache.")
        case "docker_build_cache":
            return prose(token, kindAxis, "Docker build cache",
                         "Intermediate build layers Docker keeps to speed up rebuilds.")
        case "docker_image_cache":
            return prose(token, kindAxis, "Docker images",
                         "Pulled and built images held by the Docker daemon.")
        case "unknown":
            return prose(token, kindAxis, "Unrecognised kind",
                         "Glomeris does not recognise this resource, so it is protected.")
        default:
            return unrecognised(
                token, kindAxis, "Unrecognised kind",
                "The CLI reported a resource kind this app has no wording for."
            )
        }
    }

    // MARK: - Policy reason codes (`reasons`)

    static let reasonAxis = "Reason"

    /// The 18 `ReasonCode` values, as prose. These are the "why" behind a
    /// safety class and read best as a short list of sentences, so they
    /// carry no symbol and no tone of their own — the safety badge above
    /// them already carries the tone, and repeating it per reason would
    /// turn a PROTECTED resource's explanation into a wall of red.
    static func reason(_ token: String) -> GlomerisTerm {
        switch token {
        case "protected_credential_material":
            return prose(token, reasonAxis, "Holds credentials",
                         "This location stores keys, tokens or other secrets.")
        case "protected_git_internals":
            return prose(token, reasonAxis, "Git internals",
                         "Deleting this would damage a repository's history.")
        case "protected_infra_state":
            return prose(token, reasonAxis, "Infrastructure state",
                         "State files like Terraform's that cannot be regenerated safely.")
        case "protected_persistent_volume":
            return prose(token, reasonAxis, "Persistent volume data",
                         "Data a container was given to keep, not to cache.")
        case "protected_user_documents":
            return prose(token, reasonAxis, "Your documents",
                         "This is your own content, not a tool's cache.")
        case "protected_system_path":
            return prose(token, reasonAxis, "System path",
                         "Owned by macOS. Glomeris never touches it.")
        case "protected_unsafe_mount_or_symlink":
            return prose(token, reasonAxis, "Unsafe mount or link",
                         "The path crosses a mount point or symlink, so deleting it "
                             + "could reach somewhere unintended.")
        case "protected_unknown_resource_kind":
            return prose(token, reasonAxis, "Unrecognised kind",
                         "Glomeris does not recognise this, and refuses by default "
                             + "rather than guessing.")
        case "evidence_incomplete":
            return prose(token, reasonAxis, "Evidence incomplete",
                         "Some checks did not finish, so the picture has gaps.")
        case "evidence_stale":
            return prose(token, reasonAxis, "Evidence out of date",
                         "The measurements are too old to act on.")
        case "evidence_probe_failed":
            return prose(token, reasonAxis, "A check failed",
                         "A measurement Glomeris needed could not be taken.")
        case "resource_in_active_use":
            return prose(token, reasonAxis, "In active use",
                         "Something is using this right now.")
        case "git_worktree_dirty":
            return prose(token, reasonAxis, "Uncommitted changes nearby",
                         "The surrounding repository has uncommitted work.")
        case "rebuild_cost_high":
            return prose(token, reasonAxis, "Expensive to rebuild",
                         "Recreating this would cost you real time.")
        case "owning_tool_live":
            return prose(token, reasonAxis, "Owning tool is running",
                         "The tool that manages this is currently running.")
        case "evidence_fresh_and_complete":
            return prose(token, reasonAxis, "Evidence fresh and complete",
                         "Every check finished, recently.")
        case "regenerable_by_tool":
            return prose(token, reasonAxis, "A tool can recreate it",
                         "Nothing is lost permanently by cleaning it.")
        case "no_active_use_observed":
            return prose(token, reasonAxis, "No active use seen",
                         "Nothing was observed using this.")
        default:
            return unrecognised(
                token, reasonAxis, "Unrecognised reason",
                "The CLI gave a reason this app has no wording for."
            )
        }
    }

    // MARK: - Table helpers

    private static func term(
        _ token: String,
        _ axis: String,
        _ title: String,
        _ explanation: String,
        _ symbolName: String,
        _ tone: GlomerisTone
    ) -> GlomerisTerm {
        GlomerisTerm(
            token: token,
            axis: axis,
            title: title,
            explanation: explanation,
            symbolName: symbolName,
            tone: tone
        )
    }

    private static func prose(
        _ token: String,
        _ axis: String,
        _ title: String,
        _ explanation: String
    ) -> GlomerisTerm {
        GlomerisTerm(
            token: token,
            axis: axis,
            title: title,
            explanation: explanation,
            symbolName: nil,
            tone: .neutral
        )
    }

    /// Fallback for a token no table recognises. Deliberately never
    /// `.positive` and never reassuring: a newer CLI, a corrupted field or
    /// a typo must surface as "we do not know what this is", with the raw
    /// token still shown beside it so it can be reported.
    private static func unrecognised(
        _ token: String,
        _ axis: String,
        _ title: String,
        _ explanation: String
    ) -> GlomerisTerm {
        GlomerisTerm(
            token: token,
            axis: axis,
            title: title,
            explanation: explanation,
            symbolName: "questionmark.diamond.fill",
            tone: .unknown
        )
    }
}
