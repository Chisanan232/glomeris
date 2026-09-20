//
//  GlomerisDesignSystem.swift
//  GlomerisMenuBar
//
//  HORO-1306. The shared visual grammar for the popover: spacing and type
//  scale, the card that every section sits in, the badge that renders one
//  GlomerisVocabulary term, and the loading/empty/error presentation.
//
//  Why this exists as one file rather than per-section styling: before this
//  ticket each of the four sections chose its own font sizes, spacing and
//  error colour independently, so the popover read as four small panels
//  from four different apps stacked in a column. Centralising it is also
//  what lets HORO-1308's AI Plan section arrive looking like it belongs
//  without re-deriving any of these decisions.
//
//  See the standing project rule in GlomerisMenuBarApp.swift. Everything
//  here is presentation: it maps an already-computed GlomerisTerm to a
//  symbol, a colour and a label. It reads no policy field, gates no
//  control, and decides nothing.
//
//  ---------------------------------------------------------------------
//  Colour is never the only signal
//  ---------------------------------------------------------------------
//  Every badge renders a symbol AND a word alongside its tint, so the
//  state survives greyscale, a colour-vision deficiency, and a tinted
//  menu-bar wallpaper. GlomerisVocabularyTests asserts each axis uses a
//  distinct symbol per state precisely so the tint is never load-bearing;
//  GlomerisDesignSystemTests asserts the tones stay visually distinct on
//  top of that. If you add a state, give it a symbol before you give it a
//  colour.
//
//  The tones are deliberately NOT a severity ramp. `guarded` is not
//  "worse than" caution — PROTECTED is the system working correctly, and
//  colouring a correct refusal red would teach a user that Glomeris is
//  broken every time it declines to delete their Terraform state.
//

import SwiftUI

// MARK: - Tokens

/// Spacing, type and metric tokens. Named by role rather than by value so
/// a change of mind about the type scale is one edit here.
enum GlomerisDesign {
    /// The popover is a reading column, not a window. 340pt fits a full
    /// "Safe to reclaim" badge plus a size next to it on one line, which
    /// the previous 260pt did not — it wrapped almost every status line
    /// and the wrapping was what made the panel hard to scan.
    static let popoverWidth: CGFloat = 340

    /// Gap between cards. Larger than any spacing *inside* a card, so the
    /// grouping is legible without needing a divider between every pair.
    static let sectionSpacing: CGFloat = 10

    /// Gap between rows within one card.
    static let rowSpacing: CGFloat = 6

    /// Gap between a label and its value, or between badges on a line.
    static let inlineSpacing: CGFloat = 5

    static let cardPadding: CGFloat = 10
    static let cardCornerRadius: CGFloat = 8
    static let outerPadding: CGFloat = 12

    /// Badge chip metrics.
    static let badgeCornerRadius: CGFloat = 4
    static let badgeHorizontalPadding: CGFloat = 6
    static let badgeVerticalPadding: CGFloat = 2

    /// Width reserved for the label column in the candidate detail's
    /// key/value rows, so the values align into a readable second column
    /// instead of starting at a different x for every row.
    static let detailLabelWidth: CGFloat = 132

    /// The maximum height the scrolling body may take before it scrolls.
    /// A menu-bar popover that grows past this stops being glanceable and
    /// starts covering the screen it is reporting on.
    static let maxBodyHeight: CGFloat = 520

    // MARK: Type scale
    //
    // Semantic fonts, not fixed point sizes, so the popover follows the
    // system text size instead of ignoring it.

    /// The popover's own title. One per popover.
    static let titleFont: Font = .system(.headline, design: .rounded)

    /// A card's title. Below the popover title, above everything in it.
    static let cardTitleFont: Font = .subheadline.weight(.semibold)

    /// The primary fact in a row — the thing being scanned for.
    static let primaryFont: Font = .system(.body)

    /// Supporting detail: sizes, ages, counts.
    static let secondaryFont: Font = .callout

    /// Explanations and raw tokens shown as provenance.
    static let captionFont: Font = .caption

    /// A badge's word.
    static let badgeFont: Font = .caption.weight(.medium)

    /// Paths and other verbatim strings.
    static let monospacedFont: Font = .system(.caption, design: .monospaced)
}

// MARK: - Tone

/// How a `GlomerisTone` is painted. The name is carried alongside the
/// colour because `Color`'s equality is not a useful test signal — the
/// name is what the unit tests assert distinctness on.
struct GlomerisToneStyle: Equatable {
    let name: String
    let color: Color
}

extension GlomerisTone {
    var style: GlomerisToneStyle {
        switch self {
        case .positive:
            // Reserved for AUTO_SAFE and a succeeded outcome. Nothing else
            // may read as reassuring — see
            // GlomerisVocabularyTests.testOnlyAutoSafeReadsAsPositive.
            return GlomerisToneStyle(name: "positive", color: .green)
        case .neutral:
            // Storage impact lives here, at every size. A big number is an
            // opportunity, not a hazard, and must not colour itself into a
            // safety claim.
            return GlomerisToneStyle(name: "neutral", color: .secondary)
        case .caution:
            return GlomerisToneStyle(name: "caution", color: .yellow)
        case .warning:
            return GlomerisToneStyle(name: "warning", color: .orange)
        case .critical:
            return GlomerisToneStyle(name: "critical", color: .red)
        case .guarded:
            // PROTECTED, and refusals that are the policy working. Blue
            // rather than red: this is a deliberate hold, not a fault, and
            // the user has nothing to fix.
            return GlomerisToneStyle(name: "guarded", color: .blue)
        case .unknown:
            // An evidence gap. Purple keeps it from being mistaken for
            // either "fine" or "broken" — neither is known yet.
            return GlomerisToneStyle(name: "unknown", color: .purple)
        }
    }

    var color: Color { style.color }
}

// MARK: - Badge

/// One vocabulary term as a chip: symbol, word, tinted background.
///
/// The symbol and the word are both always present. The tint is the third
/// signal, never the first.
struct GlomerisBadgeView: View {
    let term: GlomerisTerm

    /// When false the chip renders as symbol + word with no fill, for
    /// places where several terms sit on one line and three filled chips
    /// would fight each other.
    var filled: Bool = true

    var body: some View {
        HStack(spacing: 3) {
            if let symbolName = term.symbolName {
                Image(systemName: symbolName)
                    .imageScale(.small)
            }
            Text(term.title)
                .font(GlomerisDesign.badgeFont)
        }
        .foregroundStyle(term.tone.color)
        .padding(.horizontal, filled ? GlomerisDesign.badgeHorizontalPadding : 0)
        .padding(.vertical, filled ? GlomerisDesign.badgeVerticalPadding : 0)
        .background {
            if filled {
                RoundedRectangle(cornerRadius: GlomerisDesign.badgeCornerRadius)
                    .fill(term.tone.color.opacity(0.12))
            }
        }
        // The chip is one element to VoiceOver, labelled with the axis and
        // the explanation — "Asks first" alone says nothing about what asks,
        // or about what. `.help` puts the same sentence in a tooltip for
        // sighted users, so neither audience has to infer it from a glyph.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(term.accessibilityLabel)
        .help(term.explanation)
    }
}

// MARK: - Card

/// A titled container. Every popover section is one of these, which is
/// what makes the column read as one app.
struct GlomerisCard<Content: View>: View {
    let title: String

    /// Shown to the right of the title — a count, a freshness stamp. Kept
    /// on the title line so the card's first row is still the content.
    var trailing: String?

    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                    .font(GlomerisDesign.cardTitleFont)
                    // The title is the heading for everything below it, so
                    // VoiceOver's heading navigation can jump card to card.
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: GlomerisDesign.inlineSpacing)
                if let trailing {
                    Text(trailing)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }
            }
            content()
        }
        .padding(GlomerisDesign.cardPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: GlomerisDesign.cardCornerRadius)
                // A material rather than a fixed grey, so the card keeps its
                // separation from the popover behind it in both light and
                // dark appearance without two hardcoded colours.
                .fill(.quaternary.opacity(0.4))
        }
    }
}

// MARK: - Loading / empty / error

/// What a section shows when it has no rows to show.
///
/// These three are deliberately distinct presentations, because conflating
/// them is the specific way a status panel lies:
///
///   - loading is transient and must not look like a failure, or a user
///     reads a slow first `detect` as a broken install;
///   - empty is frequently GOOD NEWS — "nothing worth reclaiming" means
///     the machine is in good shape — and must not be dressed as an error;
///   - failure is the only one that may read as a problem, and it must say
///     what failed rather than showing an empty list that implies "clean".
struct GlomerisStateMessage: Equatable {
    enum Kind: Equatable {
        case loading
        case empty
        case failure
    }

    let kind: Kind
    let title: String
    let detail: String?

    /// `nil` for `loading`, which renders a spinner instead of a glyph.
    let symbolName: String?

    var tone: GlomerisTone {
        switch kind {
        case .loading: return .neutral
        case .empty: return .neutral
        case .failure: return .critical
        }
    }

    /// `subject` names the thing being waited on — "Checking disk space…"
    /// tells a user more than "Loading…" while a first `detect` runs, which
    /// on a cold cache is the slowest thing the popover does.
    static func loading(_ subject: String) -> GlomerisStateMessage {
        GlomerisStateMessage(
            kind: .loading,
            title: subject,
            detail: nil,
            symbolName: nil
        )
    }

    static func empty(_ title: String, detail: String? = nil) -> GlomerisStateMessage {
        GlomerisStateMessage(
            kind: .empty,
            title: title,
            detail: detail,
            symbolName: "checkmark.circle"
        )
    }

    /// `message` is expected to come from
    /// `SectionFetchErrors.shortMessage(_:subject:)`, which already names
    /// the subcommand and summarises the failure in one sentence.
    static func failure(_ message: String) -> GlomerisStateMessage {
        GlomerisStateMessage(
            kind: .failure,
            title: message,
            detail: nil,
            symbolName: "exclamationmark.triangle.fill"
        )
    }
}

struct GlomerisStateMessageView: View {
    let message: GlomerisStateMessage

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
            if message.kind == .loading {
                ProgressView()
                    .controlSize(.small)
            } else if let symbolName = message.symbolName {
                Image(systemName: symbolName)
                    .imageScale(.small)
                    .foregroundStyle(message.tone.color)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(message.title)
                    .font(GlomerisDesign.secondaryFont)
                    .foregroundStyle(message.kind == .failure ? message.tone.color : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = message.detail {
                    Text(detail)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

// MARK: - Rows

/// A label/value row with the labels aligned into a column.
struct GlomerisDetailRow<Value: View>: View {
    let label: String
    @ViewBuilder var value: () -> Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
            Text(label)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .frame(width: GlomerisDesign.detailLabelWidth, alignment: .leading)
            value()
        }
    }
}

/// A filesystem path, truncated in the middle with the whole thing in a
/// tooltip and in the accessibility label.
///
/// Head or tail truncation is actively misleading for these paths: the
/// interesting part of
/// `~/Library/Developer/Xcode/DerivedData/MyApp-abc123/Build` is at both
/// ends, and `…/Build` could be any project on the machine. Middle
/// truncation keeps the root and the leaf, which is what identifies it.
struct GlomerisPathText: View {
    let path: String

    var body: some View {
        Text(path)
            .font(GlomerisDesign.monospacedFont)
            .lineLimit(1)
            .truncationMode(.middle)
            .help(path)
            // VoiceOver reads the label, not the truncated rendering, so the
            // full path is still available to it.
            .accessibilityLabel("Path: \(path)")
    }
}

#Preview {
    VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
        GlomerisCard(title: "Status", trailing: "just now") {
            GlomerisBadgeView(term: GlomerisVocabulary.pressure("PRESSURED"))
            GlomerisStateMessageView(message: .loading("Checking disk space…"))
        }
        GlomerisCard(title: "Candidates", trailing: "3") {
            HStack(spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: GlomerisVocabulary.storageImpact(human: "12.4 GB", isLowerBound: true))
                GlomerisBadgeView(term: GlomerisVocabulary.safety("AUTO_SAFE"))
                GlomerisBadgeView(term: GlomerisVocabulary.confidence("high"))
            }
            HStack(spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: GlomerisVocabulary.storageImpact(human: "2 MB", isLowerBound: false))
                GlomerisBadgeView(term: GlomerisVocabulary.safety("PROTECTED"))
                GlomerisBadgeView(term: GlomerisVocabulary.completeness("partial"))
            }
            GlomerisPathText(path: "~/Library/Developer/Xcode/DerivedData/MyApp-abc123def456/Build/Products")
            GlomerisStateMessageView(message: .empty("Nothing worth reclaiming", detail: "Your disk is in good shape."))
            GlomerisStateMessageView(message: .failure("detect: the glomeris CLI was not found."))
        }
    }
    .padding(GlomerisDesign.outerPadding)
    .frame(width: GlomerisDesign.popoverWidth)
}
