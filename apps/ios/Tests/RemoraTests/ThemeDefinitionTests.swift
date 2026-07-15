import XCTest
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
}
