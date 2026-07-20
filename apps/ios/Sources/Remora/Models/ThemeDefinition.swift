import SwiftUI

// MARK: - Theme JSON model (VS Code format)

struct ThemeDefinition: Codable {
    let name: String
    let type: ThemeType
    let colors: [String: String]

    enum ThemeType: String, Codable {
        case light, dark
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case type
        case colors
    }

    init(name: String, type: ThemeType, colors: [String: String]) {
        self.name = name
        self.type = type
        self.colors = colors
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        type = try container.decode(ThemeType.self, forKey: .type)

        // VS Code themes occasionally include null entries or non-string
        // values (arrays, objects) under `colors`. Decode tolerantly and
        // drop anything that isn't a string so the rest of the app can
        // assume `[String: String]`.
        let rawColors = try container.decode([String: ThemeColorValue].self, forKey: .colors)
        colors = rawColors.compactMapValues { value in
            value.string.map(Self.sanitizeHex)
        }
    }

    // VS Code allows shorthand RGB/RGBA and #RRGGBBAA. Downstream color
    // helpers assume 6-digit RGB, so normalize shorthand and strip alpha at
    // the decode boundary.
    private static func sanitizeHex(_ raw: String) -> String {
        guard raw.hasPrefix("#") else { return raw }
        switch raw.count {
        case 4, 5:
            let digits = Array(raw.dropFirst())
            return "#" + String(digits.prefix(3).flatMap { [$0, $0] })
        case 9:
            return String(raw.prefix(7))
        default:
            return raw
        }
    }

    // tokenColors are ignored — syntax highlighting is handled by Hairball
}

private struct ThemeColorValue: Decodable {
    let string: String?

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            string = nil
        } else {
            // Only string values are usable as theme colors; arrays,
            // objects, numbers, etc. fall through as nil and get dropped.
            string = try? container.decode(String.self)
        }
    }
}

// MARK: - Lightweight index entry for picker UI

struct ThemeIndexEntry: Codable, Identifiable {
    let slug: String
    let name: String
    let type: ThemeDefinition.ThemeType
    let accentHex: String
    let backgroundHex: String
    let foregroundHex: String

    var id: String { slug }
}

// MARK: - Resolved theme (app-ready hex values)

struct ResolvedTheme {
    let slug: String
    let name: String
    let type: ThemeDefinition.ThemeType

    let background: String
    let surface: String
    let surfaceLight: String
    let textPrimary: String
    let textSecondary: String
    let textMuted: String
    let textBody: String
    let textSystem: String
    let accent: String
    let accentForeground: String
    let accentForegroundOnSurface: String
    let accentStrong: String
    let border: String
    let separator: String
    let danger: String
    let success: String
    let warning: String
    let textOnAccent: String
    let codeBackground: String

    init(slug: String, definition d: ThemeDefinition) {
        self.slug = slug
        self.name = d.name
        self.type = d.type
        let c = d.colors

        let bg = c["editor.background"] ?? (d.type == .dark ? "#111111" : "#FFFFFF")
        let fg = c["editor.foreground"] ?? (d.type == .dark ? "#FFFFFF" : "#1A1A1A")

        self.background = bg
        self.textPrimary = fg
        let candidateSurface = c["sideBar.background"]
            ?? Self.adjustBrightness(bg, by: d.type == .dark ? 0.03 : -0.02)
        let candidateSurfaceLight = c["activityBar.background"]
            ?? Self.adjustBrightness(candidateSurface, by: d.type == .dark ? 0.04 : -0.03)
        let candidateChrome = [bg, candidateSurface, candidateSurfaceLight]

        // App chrome mixes these three surfaces in ways VS Code itself does
        // not. Reject a theme's sidebar/activity colors only when their
        // luminance polarity makes one AA-readable semantic foreground
        // mathematically impossible across the set (for example a white
        // editor paired with nearly black sidebars). This keeps the bundled
        // theme usable without proliferating context-fragile text roles.
        if Self.hasSharedAAForeground(against: candidateChrome) {
            self.surface = candidateSurface
            self.surfaceLight = candidateSurfaceLight
        } else {
            let fallbackSurface = Self.adjustBrightness(bg, by: d.type == .dark ? 0.03 : -0.02)
            let fallbackSurfaceLight = Self.adjustBrightness(
                fallbackSurface,
                by: d.type == .dark ? 0.04 : -0.03
            )
            if Self.hasSharedAAForeground(
                against: [bg, fallbackSurface, fallbackSurfaceLight]
            ) {
                self.surface = fallbackSurface
                self.surfaceLight = fallbackSurfaceLight
            } else {
                self.surface = bg
                self.surfaceLight = bg
            }
        }

        let chromeSurfaces = [self.background, self.surface, self.surfaceLight]

        let rawSecondary = c["sideBar.foreground"] ?? Self.dimColor(fg, factor: 0.55)
        self.textSecondary = Self.contrastSafeColor(
            rawSecondary,
            against: chromeSurfaces,
            preferredTarget: fg
        )
        let rawMuted = c["editorLineNumber.foreground"] ?? Self.dimColor(fg, factor: 0.35)
        self.textMuted = Self.contrastSafeColor(
            rawMuted,
            against: chromeSurfaces,
            preferredTarget: fg
        )
        let rawBody = Self.dimColor(fg, factor: 0.88)
        self.textBody = Self.contrastSafeColor(
            rawBody,
            against: [self.background],
            preferredTarget: fg
        )
        let rawSystem = Self.dimColor(fg, factor: 0.7)
        self.textSystem = Self.contrastSafeColor(
            rawSystem,
            against: chromeSurfaces,
            preferredTarget: fg
        )
        self.accent = c["textLink.foreground"]
            ?? c["button.background"]
            ?? (d.type == .dark ? "#B0B0B0" : "#4A4A4A")
        self.accentForeground = Self.contrastSafeColor(
            self.accent,
            against: [self.background],
            preferredTarget: fg
        )
        self.accentForegroundOnSurface = Self.contrastSafeColor(
            self.accent,
            against: chromeSurfaces,
            preferredTarget: fg
        )
        self.accentStrong = c["button.background"] ?? c["textLink.foreground"] ?? self.accent
        self.border = c["editorGroup.border"]
            ?? c["sideBar.border"]
            ?? Self.adjustBrightness(self.surface, by: d.type == .dark ? 0.05 : -0.05)
        self.separator = c["panel.border"]
            ?? Self.adjustBrightness(bg, by: d.type == .dark ? 0.04 : -0.04)
        self.danger = d.type == .dark ? "#FF5555" : "#D32F2F"
        self.success = d.type == .dark ? "#6EA676" : "#2E7D32"
        self.warning = d.type == .dark ? "#E2A644" : "#A84400"
        self.codeBackground = bg

        // Selected controls use `accent`, not `accentStrong`, as their fill.
        // Derive the semantic label from that exact rendered background.
        self.textOnAccent = Self.contrastForeground(on: self.accent)

    }

    // MARK: - Color utilities

    static func brightness(of hex: String) -> Double {
        let (r, g, b) = hexToRGB(hex)
        return 0.299 * r + 0.587 * g + 0.114 * b
    }

    /// Keep the theme's hue whenever possible, moving only as far toward its
    /// own primary text (or a neutral endpoint) as needed to meet WCAG AA on
    /// every standard app-chrome surface where these semantic roles appear.
    static func contrastSafeColor(
        _ candidate: String,
        against backgrounds: [String],
        preferredTarget: String,
        minimumRatio: Double = 4.5
    ) -> String {
        guard minimumContrast(of: candidate, against: backgrounds) < minimumRatio else {
            return candidate
        }

        let candidateRGB = hexToRGB(candidate)
        let targets = [preferredTarget, "#000000", "#FFFFFF"]
        var best: (hex: String, distance: Double)?

        for target in targets {
            guard minimumContrast(of: target, against: backgrounds) >= minimumRatio else {
                continue
            }
            let targetRGB = hexToRGB(target)
            for step in 1...1_000 {
                let amount = Double(step) / 1_000
                let blended = rgbToHex(
                    candidateRGB.0 + (targetRGB.0 - candidateRGB.0) * amount,
                    candidateRGB.1 + (targetRGB.1 - candidateRGB.1) * amount,
                    candidateRGB.2 + (targetRGB.2 - candidateRGB.2) * amount
                )
                guard minimumContrast(of: blended, against: backgrounds) >= minimumRatio else {
                    continue
                }
                let resolvedRGB = hexToRGB(blended)
                let distance = pow(resolvedRGB.0 - candidateRGB.0, 2)
                    + pow(resolvedRGB.1 - candidateRGB.1, 2)
                    + pow(resolvedRGB.2 - candidateRGB.2, 2)
                if best == nil || distance < best!.distance {
                    best = (blended, distance)
                }
                break
            }
        }

        return best?.hex ?? contrastForeground(on: backgrounds.first ?? "#000000")
    }

    static func contrastForeground(on background: String, minimumRatio: Double = 4.5) -> String {
        let nearBlack = "#0D0D0D"
        let white = "#FFFFFF"
        let nearBlackRatio = contrastRatio(nearBlack, background)
        let whiteRatio = contrastRatio(white, background)
        if max(nearBlackRatio, whiteRatio) >= minimumRatio {
            return nearBlackRatio >= whiteRatio ? nearBlack : white
        }
        return contrastRatio("#000000", background) >= whiteRatio ? "#000000" : white
    }

    static func contrastRatio(_ foreground: String, _ background: String) -> Double {
        let first = relativeLuminance(foreground)
        let second = relativeLuminance(background)
        return (max(first, second) + 0.05) / (min(first, second) + 0.05)
    }

    private static func minimumContrast(of foreground: String, against backgrounds: [String]) -> Double {
        backgrounds.map { contrastRatio(foreground, $0) }.min() ?? .infinity
    }

    private static func hasSharedAAForeground(against backgrounds: [String]) -> Bool {
        minimumContrast(of: "#000000", against: backgrounds) >= 4.5
            || minimumContrast(of: "#FFFFFF", against: backgrounds) >= 4.5
    }

    private static func relativeLuminance(_ hex: String) -> Double {
        let (r, g, b) = hexToRGB(hex)
        func linear(_ channel: Double) -> Double {
            channel <= 0.04045
                ? channel / 12.92
                : pow((channel + 0.055) / 1.055, 2.4)
        }
        return (0.2126 * linear(r)) + (0.7152 * linear(g)) + (0.0722 * linear(b))
    }

    static func adjustBrightness(_ hex: String, by amount: Double) -> String {
        let (r, g, b) = hexToRGB(hex)
        let nr = min(1, max(0, r + amount))
        let ng = min(1, max(0, g + amount))
        let nb = min(1, max(0, b + amount))
        return rgbToHex(nr, ng, nb)
    }

    static func dimColor(_ hex: String, factor: Double) -> String {
        let (r, g, b) = hexToRGB(hex)
        let brightness = 0.299 * r + 0.587 * g + 0.114 * b
        if brightness > 0.5 {
            // Light foreground on dark bg — dim toward black
            return rgbToHex(r * factor, g * factor, b * factor)
        } else {
            // Dark foreground on light bg — dim toward white
            let inv = 1.0 - factor
            return rgbToHex(r + (1 - r) * inv, g + (1 - g) * inv, b + (1 - b) * inv)
        }
    }

    static func hexToRGB(_ hex: String) -> (Double, Double, Double) {
        let cleaned = hex.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        var int: UInt64 = 0
        Scanner(string: cleaned).scanHexInt64(&int)
        let r = Double((int >> 16) & 0xFF) / 255
        let g = Double((int >> 8) & 0xFF) / 255
        let b = Double(int & 0xFF) / 255
        return (
            r,
            g,
            b
        )
    }

    static func rgbToHex(_ r: Double, _ g: Double, _ b: Double) -> String {
        String(format: "#%02X%02X%02X", Int(r * 255), Int(g * 255), Int(b * 255))
    }
}

// MARK: - Default themes (fallback when no JSON loaded)

extension ResolvedTheme {
    static let defaultLight = ResolvedTheme(
        slug: "remora-light",
        definition: ThemeDefinition(name: "Remora Light", type: .light, colors: [
            "editor.background": "#F7FCFE", "editor.foreground": "#102A36",
            "sideBar.background": "#EAF7FB", "sideBar.foreground": "#365866",
            "activityBar.background": "#DCEFF5",
            "editorLineNumber.foreground": "#4E6671",
            "editorGroup.border": "#B7DCE7", "panel.border": "#D8E8F1",
            "textLink.foreground": "#036F8F", "button.background": "#036F8F",
            "gitDecoration.addedResourceForeground": "#00A240",
            "gitDecoration.deletedResourceForeground": "#E02E2A",
        ])
    )

    static let defaultDark = ResolvedTheme(
        slug: "remora-dark",
        definition: ThemeDefinition(name: "Remora", type: .dark, colors: [
            "editor.background": "#02082C", "editor.foreground": "#EAFBFF",
            "sideBar.background": "#011B44", "sideBar.foreground": "#A8DCEB",
            "activityBar.background": "#022753",
            "editorLineNumber.foreground": "#83AFC2",
            "editorGroup.border": "#044875", "panel.border": "#02356A",
            "textLink.foreground": "#0DD5F0", "button.background": "#07F2FB",
            "gitDecoration.addedResourceForeground": "#00A240",
            "gitDecoration.deletedResourceForeground": "#E02E2A",
        ])
    )
}
