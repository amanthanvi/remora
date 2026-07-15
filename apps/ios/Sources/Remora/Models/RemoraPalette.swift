import SwiftUI

/// Shared color palette used by both the main app (RemoraTheme) and the
/// Shared application palette.
/// UserDefaults (written by ThemeManager) with hardcoded fallbacks.
enum RemoraPalette {
    // MARK: - Adaptive pairs (light, dark)

    struct Pair {
        let light: String
        let dark: String
    }

    static let appGroupSuite = "group.com.remora.app"
    private static let shared = UserDefaults(suiteName: appGroupSuite)

    private static func pair(_ key: String, lightFallback: String, darkFallback: String) -> Pair {
        Pair(
            light: shared?.string(forKey: "theme.light.\(key)") ?? lightFallback,
            dark: shared?.string(forKey: "theme.dark.\(key)") ?? darkFallback
        )
    }

    static var accent: Pair        { pair("accent", lightFallback: "#036F8F", darkFallback: "#0DD5F0") }
    static var accentStrong: Pair   { pair("accentStrong", lightFallback: "#036F8F", darkFallback: "#07F2FB") }
    static var textPrimary: Pair    { pair("textPrimary", lightFallback: "#102A36", darkFallback: "#EAFBFF") }
    static var textSecondary: Pair  { pair("textSecondary", lightFallback: "#365866", darkFallback: "#A8DCEB") }
    static var textMuted: Pair      { pair("textMuted", lightFallback: "#4E6671", darkFallback: "#83AFC2") }
    static var textBody: Pair       { pair("textBody", lightFallback: "#294653", darkFallback: "#CEE3EA") }
    static var textSystem: Pair     { pair("textSystem", lightFallback: "#486674", darkFallback: "#A4B7C1") }
    static var surface: Pair        { pair("surface", lightFallback: "#EAF7FB", darkFallback: "#011B44") }
    static var surfaceLight: Pair   { pair("surfaceLight", lightFallback: "#DCEFF5", darkFallback: "#022753") }
    static var border: Pair         { pair("border", lightFallback: "#B7DCE7", darkFallback: "#044875") }
    static var separator: Pair      { pair("separator", lightFallback: "#D8E8F1", darkFallback: "#02356A") }
    static var danger: Pair         { pair("danger", lightFallback: "#D32F2F", darkFallback: "#FF5555") }
    static var success: Pair        { pair("success", lightFallback: "#2E7D32", darkFallback: "#6EA676") }
    static var warning: Pair        { pair("warning", lightFallback: "#A84400", darkFallback: "#E2A644") }
    static var textOnAccent: Pair   { pair("textOnAccent", lightFallback: "#FFFFFF", darkFallback: "#0D0D0D") }
    static var codeBackground: Pair { pair("codeBackground", lightFallback: "#F7FCFE", darkFallback: "#02082C") }

    // MARK: - Font

    /// Whether the user prefers monospaced font. Reads from the shared App Group.
    static var isMono: Bool {
        let raw = shared?.string(forKey: "fontFamily") ?? "mono"
        return raw == "mono"
    }

    /// Font design matching the user's font preference.
    static var fontDesign: Font.Design {
        isMono ? .monospaced : .default
    }
}

// MARK: - SwiftUI helpers for widget / preview use

extension RemoraPalette.Pair {
    /// Resolve to a SwiftUI `Color` using the SwiftUI color scheme
    /// (works in widgets and previews, unlike UITraitCollection).
    func color(for scheme: ColorScheme) -> Color {
        Self.colorFromHex(scheme == .dark ? dark : light)
    }

    static func colorFromHex(_ hex: String) -> Color {
        let hex = hex.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        var int: UInt64 = 0
        Scanner(string: hex).scanHexInt64(&int)
        let r = Double((int >> 16) & 0xFF) / 255
        let g = Double((int >> 8) & 0xFF) / 255
        let b = Double(int & 0xFF) / 255
        return Color(red: r, green: g, blue: b)
    }
}
