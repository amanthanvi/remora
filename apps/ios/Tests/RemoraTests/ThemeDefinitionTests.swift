import XCTest
import SwiftUI
import UIKit
@testable import Remora

final class ThemeDefinitionTests: XCTestCase {
    func testThemeDefinitionIgnoresNullAndNonStringColorEntries() throws {
        let data = Data(
            """
            {
              "name": "night-owl",
              "type": "dark",
              "colors": {
                "editor.background": "#011627",
                "editor.findRangeHighlightBackground": null,
                "editor.foreground": "#d6deeb",
                "symbolIcon.constantForeground": ["#79c0ff", "#d2a8ff"]
              }
            }
            """.utf8
        )

        let theme = try JSONDecoder().decode(ThemeDefinition.self, from: data)

        XCTAssertEqual(theme.colors["editor.background"], "#011627")
        XCTAssertEqual(theme.colors["editor.foreground"], "#d6deeb")
        XCTAssertNil(theme.colors["editor.findRangeHighlightBackground"])
        XCTAssertNil(theme.colors["symbolIcon.constantForeground"])
    }

    func testThemeDefinitionStripsAlphaFromEightDigitHex() throws {
        let data = Data(
            """
            {
              "name": "alpha-theme",
              "type": "dark",
              "colors": {
                "button.background": "#7e57c2cc",
                "editor.background": "#011627"
              }
            }
            """.utf8
        )

        let theme = try JSONDecoder().decode(ThemeDefinition.self, from: data)

        // 8-digit #RRGGBBAA is sanitized to 6-digit #RRGGBB at decode time
        // so downstream color helpers see a well-formed RGB string.
        XCTAssertEqual(theme.colors["button.background"], "#7e57c2")
        XCTAssertEqual(theme.colors["editor.background"], "#011627")
    }

    func testThemeDefinitionNormalizesShorthandHexAndDropsShorthandAlpha() throws {
        let data = Data(
            """
            {
              "name": "shorthand-theme",
              "type": "light",
              "colors": {
                "editor.background": "#fff",
                "editor.foreground": "#1238",
                "textLink.foreground": "#AbC"
              }
            }
            """.utf8
        )

        let theme = try JSONDecoder().decode(ThemeDefinition.self, from: data)

        XCTAssertEqual(theme.colors["editor.background"], "#ffffff")
        XCTAssertEqual(theme.colors["editor.foreground"], "#112233")
        XCTAssertEqual(theme.colors["textLink.foreground"], "#AAbbCC")
    }

    func testDefaultThemesUseRemoraOceanPalette() {
        XCTAssertEqual(ResolvedTheme.defaultDark.slug, "remora-dark")
        XCTAssertEqual(ResolvedTheme.defaultDark.background, "#02082C")
        XCTAssertEqual(ResolvedTheme.defaultDark.accent, "#0DD5F0")
        XCTAssertEqual(ResolvedTheme.defaultDark.accentStrong, "#07F2FB")
        XCTAssertEqual(ResolvedTheme.defaultDark.textMuted, "#83AFC2")
        XCTAssertEqual(ResolvedTheme.defaultDark.warning, "#E2A644")
        XCTAssertEqual(ResolvedTheme.defaultDark.textOnAccent, "#0D0D0D")

        XCTAssertEqual(ResolvedTheme.defaultLight.slug, "remora-light")
        XCTAssertEqual(ResolvedTheme.defaultLight.background, "#F7FCFE")
        XCTAssertEqual(ResolvedTheme.defaultLight.accent, "#036F8F")
        XCTAssertEqual(ResolvedTheme.defaultLight.accentStrong, "#036F8F")
        XCTAssertEqual(ResolvedTheme.defaultLight.textMuted, "#4E6671")
        XCTAssertEqual(ResolvedTheme.defaultLight.warning, "#A84400")
        XCTAssertEqual(ResolvedTheme.defaultLight.textOnAccent, "#FFFFFF")
    }

    func testDefaultSubduedTextRolesMeetAAContrast() {
        let pairs = [
            (ResolvedTheme.defaultLight.textMuted, ResolvedTheme.defaultLight.background),
            (ResolvedTheme.defaultLight.textSecondary, ResolvedTheme.defaultLight.surface),
            (ResolvedTheme.defaultLight.textSecondary, ResolvedTheme.defaultLight.surfaceLight),
            (ResolvedTheme.defaultDark.textMuted, ResolvedTheme.defaultDark.background),
            (ResolvedTheme.defaultDark.textSecondary, ResolvedTheme.defaultDark.surface),
            (ResolvedTheme.defaultDark.textSecondary, ResolvedTheme.defaultDark.surfaceLight),
        ]

        for (foreground, background) in pairs {
            XCTAssertGreaterThanOrEqual(
                contrastRatio(foreground, background),
                4.5,
                "Expected \(foreground) on \(background) to meet WCAG AA"
            )
        }
    }

    func testDefaultAccentLabelsMeetAAContrast() {
        let pairs = [
            (ResolvedTheme.defaultLight.textOnAccent, ResolvedTheme.defaultLight.accent),
            (ResolvedTheme.defaultLight.accentForeground, ResolvedTheme.defaultLight.background),
            (ResolvedTheme.defaultLight.accentForegroundOnSurface, ResolvedTheme.defaultLight.surface),
            (ResolvedTheme.defaultLight.accentForegroundOnSurface, ResolvedTheme.defaultLight.surfaceLight),
            (ResolvedTheme.defaultDark.textOnAccent, ResolvedTheme.defaultDark.accent),
            (ResolvedTheme.defaultDark.accentForeground, ResolvedTheme.defaultDark.background),
            (ResolvedTheme.defaultDark.accentForegroundOnSurface, ResolvedTheme.defaultDark.surface),
            (ResolvedTheme.defaultDark.accentForegroundOnSurface, ResolvedTheme.defaultDark.surfaceLight),
        ]

        for (foreground, background) in pairs {
            XCTAssertGreaterThanOrEqual(
                contrastRatio(foreground, background),
                4.5,
                "Expected accent label \(foreground) on \(background) to meet WCAG AA"
            )
        }
    }

    func testEveryBundledThemeSemanticForegroundMeetsAAContrast() throws {
        let manifestURL = try XCTUnwrap(
            Bundle.main.url(forResource: "theme-manifest", withExtension: "json")
        )
        let entries = try JSONDecoder().decode(
            [ThemeIndexEntry].self,
            from: Data(contentsOf: manifestURL)
        )
        XCTAssertEqual(entries.count, 78, "Expected the complete bundled theme manifest")

        for entry in entries {
            let definitionURL = try XCTUnwrap(
                Bundle.main.url(forResource: entry.slug, withExtension: "json"),
                "Missing bundled theme file for \(entry.slug)"
            )
            let definition = try JSONDecoder().decode(
                ThemeDefinition.self,
                from: Data(contentsOf: definitionURL)
            )
            let theme = ResolvedTheme(slug: entry.slug, definition: definition)
            let rolePairs: [(role: String, foreground: String, background: String)] = [
                ("textMuted/background", theme.textMuted, theme.background),
                ("textMuted/surface", theme.textMuted, theme.surface),
                ("textMuted/surfaceLight", theme.textMuted, theme.surfaceLight),
                ("textBody/background", theme.textBody, theme.background),
                ("textSystem/background", theme.textSystem, theme.background),
                ("textSystem/surface", theme.textSystem, theme.surface),
                ("textSystem/surfaceLight", theme.textSystem, theme.surfaceLight),
                ("textSecondary/background", theme.textSecondary, theme.background),
                ("textSecondary/surface", theme.textSecondary, theme.surface),
                ("textSecondary/surfaceLight", theme.textSecondary, theme.surfaceLight),
                ("accentForeground/background", theme.accentForeground, theme.background),
                ("accentForegroundOnSurface/background", theme.accentForegroundOnSurface, theme.background),
                ("accentForegroundOnSurface/surface", theme.accentForegroundOnSurface, theme.surface),
                ("accentForegroundOnSurface/surfaceLight", theme.accentForegroundOnSurface, theme.surfaceLight),
                ("textOnAccent/accent", theme.textOnAccent, theme.accent),
            ]

            for pair in rolePairs {
                XCTAssertGreaterThanOrEqual(
                    ResolvedTheme.contrastRatio(pair.foreground, pair.background),
                    4.5,
                    "\(entry.slug) \(pair.role): \(pair.foreground) on \(pair.background)"
                )
            }
        }
    }

    func testAccentForegroundConsumersStaySeparateFromDecorativeAccentColors() throws {
        let viewsDirectory = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources/Remora/Views", isDirectory: true)

        func source(_ fileName: String) throws -> String {
            try String(
                contentsOf: viewsDirectory.appendingPathComponent(fileName),
                encoding: .utf8
            )
        }

        let voiceCall = try source("VoiceCallView.swift")
        XCTAssertTrue(voiceCall.contains(".foregroundColor(RemoraTheme.textOnDarkOverlay)"))
        XCTAssertTrue(voiceCall.contains(".background(Capsule().fill(Color(hex: \"#02082C\")))"))
        XCTAssertFalse(voiceCall.contains("phaseForegroundColor"))
        XCTAssertTrue(voiceCall.contains(".foregroundColor(titleForegroundColor)"))
        XCTAssertTrue(voiceCall.contains("return RemoraTheme.accentForeground\n"))

        let toolCall = try source("ToolCallCardView.swift")
        XCTAssertTrue(toolCall.contains(".foregroundColor(kindForeground)"))
        XCTAssertFalse(toolCall.contains(".foregroundColor(kindAccent)"))
        XCTAssertTrue(toolCall.contains(".fill(item.index == identifiedItems.count - 1 ? kindAccent"))
        XCTAssertTrue(toolCall.contains("return RemoraTheme.accentForegroundOnSurface"))

        let timeline = try source("ConversationTimelineDetailRows.swift")
        XCTAssertTrue(timeline.contains(".foregroundColor(headerForegroundColor)"))
        XCTAssertTrue(timeline.contains("return RemoraTheme.accentForegroundOnSurface"))

        let inlineVoice = try source("InlineVoiceStatusStrip.swift")
        XCTAssertTrue(inlineVoice.contains(".foregroundColor(phaseForegroundColor(session.phase))"))
        XCTAssertTrue(inlineVoice.contains("return RemoraTheme.accentForegroundOnSurface"))

        let realtimeVoice = try source("RealtimeVoiceScreen.swift")
        XCTAssertTrue(realtimeVoice.contains(".foregroundColor(phaseForegroundColor)"))
        XCTAssertFalse(realtimeVoice.contains(".foregroundColor(phaseDecorativeColor)"))
        XCTAssertTrue(realtimeVoice.contains("color: phaseDecorativeColor"))
        XCTAssertTrue(realtimeVoice.contains("tint: phaseDecorativeColor"))
        XCTAssertTrue(realtimeVoice.contains("return Color(hex: glowPalette.accentForeground)"))
    }

    func testSFMonoPrecedesBundledFallbackForAppChrome() {
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .regular),
            ["SFMono-Regular", "BerkeleyMono-Regular"]
        )
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .light),
            ["SFMono-Light", "SFMono-Regular", "BerkeleyMono-Regular"]
        )
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .medium),
            ["SFMono-Medium", "SFMono-Regular", "BerkeleyMono-Regular"]
        )
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .semibold),
            [
                "SFMono-Semibold",
                "SFMono-Bold",
                "SFMono-Medium",
                "SFMono-Regular",
                "BerkeleyMono-Bold",
                "BerkeleyMono-Regular",
            ]
        )
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .bold),
            [
                "SFMono-Bold",
                "SFMono-Semibold",
                "SFMono-Medium",
                "SFMono-Regular",
                "BerkeleyMono-Bold",
                "BerkeleyMono-Regular",
            ]
        )
        XCTAssertEqual(
            RemoraFont.monoFontCandidates(weight: .heavy),
            RemoraFont.monoFontCandidates(weight: .bold)
        )
    }

    func testMonoFontResolverReturnsAnInstalledCandidateForEverySupportedWeight() throws {
        let weights: [Font.Weight] = [.light, .regular, .medium, .semibold, .bold]

        for weight in weights {
            let name = try XCTUnwrap(RemoraFont.resolvedMonoFontName(weight: weight))
            XCTAssertTrue(RemoraFont.monoFontCandidates(weight: weight).contains(name))
            XCTAssertNotNil(UIFont(name: name, size: 17), "Expected \(name) to resolve at runtime")
        }
    }

    func testContextBadgeClampsRenderedAndAnnouncedPercentage() {
        XCTAssertEqual(ContextBadgeView.clampedPercent(-1), 0)
        XCTAssertEqual(ContextBadgeView.clampedPercent(37), 37)
        XCTAssertEqual(ContextBadgeView.clampedPercent(101), 100)

        XCTAssertEqual(ContextBadgeView(percent: -20, tint: .blue).percent, 0)
        XCTAssertEqual(ContextBadgeView(percent: 140, tint: .blue).percent, 100)
    }

    func testCompactHomeToolbarCollapsesSupporterBadges() {
        XCTAssertEqual(
            HomeDashboardView.supporterBadgeToolbarLayout(
                chrome: .full,
                horizontalSizeClass: .compact
            ),
            .compact
        )
        XCTAssertEqual(
            HomeDashboardView.supporterBadgeToolbarLayout(
                chrome: .full,
                horizontalSizeClass: .regular
            ),
            .expanded
        )
        XCTAssertEqual(
            HomeDashboardView.supporterBadgeToolbarLayout(
                chrome: .sidebar,
                horizontalSizeClass: .compact
            ),
            .logoOnly
        )
    }

    func testThreadSearchAccessibilityLabelsDescribeThePendingAction() {
        XCTAssertEqual(
            ThreadSearchAccessibility.pinActionLabel(threadTitle: "Fix pairing", isPinned: false),
            "Pin Fix pairing"
        )
        XCTAssertEqual(
            ThreadSearchAccessibility.pinActionLabel(threadTitle: "Fix pairing", isPinned: true),
            "Unpin Fix pairing"
        )
        XCTAssertEqual(
            ThreadSearchAccessibility.branchesActionLabel(count: 3, isExpanded: false),
            "Expand 3 branches"
        )
        XCTAssertEqual(
            ThreadSearchAccessibility.branchesActionLabel(count: 3, isExpanded: true),
            "Collapse 3 branches"
        )
    }

    func testReduceMotionSuppressesInteractiveAnimation() {
        XCTAssertNil(RemoraMotionPolicy.animation(.easeInOut(duration: 0.2), reduceMotion: true))
        XCTAssertNotNil(RemoraMotionPolicy.animation(.easeInOut(duration: 0.2), reduceMotion: false))
    }

    func testSavedAppToolbarStacksForCompactWidthOrAccessibilityText() {
        XCTAssertEqual(
            SavedAppDetailView.toolbarLayout(
                dynamicTypeSize: .large,
                horizontalSizeClass: .regular
            ),
            .inline
        )
        XCTAssertEqual(
            SavedAppDetailView.toolbarLayout(
                dynamicTypeSize: .large,
                horizontalSizeClass: .compact
            ),
            .stacked
        )
        XCTAssertEqual(
            SavedAppDetailView.toolbarLayout(
                dynamicTypeSize: .accessibility1,
                horizontalSizeClass: .regular
            ),
            .stacked
        )
    }

    func testConversationHeaderUsesCompactToolbarLayoutAtAccessibilitySizes() {
        XCTAssertEqual(
            HeaderView.toolbarLayout(dynamicTypeSize: .large, textScale: 1),
            .expanded
        )
        XCTAssertEqual(
            HeaderView.toolbarLayout(dynamicTypeSize: .xxxLarge, textScale: 1),
            .compact
        )
        XCTAssertEqual(
            HeaderView.toolbarLayout(dynamicTypeSize: .xxxLarge, textScale: 1.8),
            .iconOnly
        )
        XCTAssertEqual(
            HeaderView.toolbarLayout(dynamicTypeSize: .accessibility1, textScale: 1),
            .iconOnly
        )
    }

    @MainActor
    func testComposerControlsKeepMinimumHitTargetsAndReflowAtAccessibilitySizes() {
        for mode in [AppModeKind.default, .plan] {
            let host = UIHostingController(
                rootView: ConversationComposerModeChip(mode: mode, onTap: {})
                    .environment(\.dynamicTypeSize, .accessibility5)
            )
            let size = host.sizeThatFits(in: CGSize(width: 220, height: 1_000))
            XCTAssertGreaterThanOrEqual(size.height, RemoraAccessibilityMetrics.minimumHitTarget)
            XCTAssertLessThanOrEqual(size.width, 220)
        }

        let planHost = UIHostingController(
            rootView: PlanImplementationPromptView(onImplement: {}, onDismiss: {})
                .environment(\.dynamicTypeSize, .accessibility5)
        )
        let planSize = planHost.sizeThatFits(in: CGSize(width: 180, height: 1_000))
        XCTAssertLessThanOrEqual(planSize.width, 180)
        XCTAssertGreaterThanOrEqual(
            planSize.height,
            RemoraAccessibilityMetrics.minimumHitTarget * 2
        )

        let contextHost = UIHostingController(
            rootView: ConversationComposerContextBarView(
                rateLimits: nil,
                contextPercent: 42
            )
            .environment(\.dynamicTypeSize, .accessibility5)
            .environment(\.textScale, 1.8)
        )
        let contextSize = contextHost.sizeThatFits(in: CGSize(width: 180, height: 1_000))
        XCTAssertLessThanOrEqual(contextSize.width, 180)
    }

    private func contrastRatio(_ first: String, _ second: String) -> Double {
        let firstLuminance = relativeLuminance(first)
        let secondLuminance = relativeLuminance(second)
        return (max(firstLuminance, secondLuminance) + 0.05)
            / (min(firstLuminance, secondLuminance) + 0.05)
    }

    private func relativeLuminance(_ hex: String) -> Double {
        let normalized = hex.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        let value = UInt64(normalized, radix: 16) ?? 0
        let components = [
            Double((value >> 16) & 0xFF) / 255,
            Double((value >> 8) & 0xFF) / 255,
            Double(value & 0xFF) / 255,
        ]
        let linear = components.map { component in
            component <= 0.04045
                ? component / 12.92
                : pow((component + 0.055) / 1.055, 2.4)
        }
        return (0.2126 * linear[0]) + (0.7152 * linear[1]) + (0.0722 * linear[2])
    }
}
