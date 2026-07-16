package com.remora.android.ui.terminal

import androidx.compose.ui.graphics.Color
import com.remora.android.state.TerminalSessionController
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertSame
import org.junit.Test
import uniffi.codex_mobile_client.TerminalConfig
import uniffi.codex_mobile_client.TerminalCursorStyle
import uniffi.codex_mobile_client.TerminalThemePreset

class TerminalConfigPrefsTest {
    @Test
    fun defaultsToSharedRemoraDarkPreset() {
        assertEquals(TerminalThemeChoice.REMORA_DARK, TerminalThemeChoice.DEFAULT)
        assertEquals(TerminalThemeChoice.REMORA_DARK, TerminalVisualDefaults.chromeTheme)
        assertEquals(TerminalThemePreset.RemoraDark, TerminalThemeChoice.DEFAULT.toPreset())
    }

    @Test
    fun nativeGridUsesAnAvailableAndroidFontFamily() {
        assertEquals("monospace", TerminalVisualDefaults.nativeGridFontFamily)
        assertNotEquals("SFMono-Regular", TerminalVisualDefaults.nativeGridFontFamily)
    }

    @Test
    fun latestConfigReplaysAcrossRendererRecreationWithoutBeingConsumed() {
        val replayState = TerminalConfigReplayState()
        val first = config(TerminalThemePreset.RemoraDark)
        val replacement = config(TerminalThemePreset.CatppuccinFrappe)
        val latest = config(TerminalThemePreset.Solarized(dark = false))
        val firstRenderer = RecordingConfigTarget()
        val recreatedRenderer = RecordingConfigTarget()

        replayState.update(first)
        replayState.rendererCreated(firstRenderer)
        replayState.update(replacement)

        assertEquals(2, firstRenderer.applied.size)
        assertSame(first, firstRenderer.applied[0])
        assertSame(replacement, firstRenderer.applied[1])

        replayState.rendererDestroyed(firstRenderer)
        replayState.update(latest)
        replayState.rendererCreated(recreatedRenderer)

        assertEquals(2, firstRenderer.applied.size)
        assertEquals(1, recreatedRenderer.applied.size)
        assertSame(latest, recreatedRenderer.applied.single())
    }

    @Test
    fun composeFallbackAndStatusColorsFollowEverySelectedPalette() {
        val cases = listOf(
            PaletteCase(
                TerminalThemeChoice.REMORA_DARK,
                "#02082C",
                "#EAFBFF",
                "#000000",
                "#EAFBFF",
            ),
            PaletteCase(
                TerminalThemeChoice.CATPPUCCIN_FRAPPE,
                "#303446",
                "#C6D0F5",
                "#51576D",
                "#C6D0F5",
            ),
            PaletteCase(
                TerminalThemeChoice.CATPPUCCIN_FRAPPE_LIGHT,
                "#EFF1F5",
                "#4C4F69",
                "#51576D",
                "#51576D",
            ),
            PaletteCase(
                TerminalThemeChoice.SOLARIZED_DARK,
                "#002B36",
                "#839496",
                "#073642",
                "#839496",
            ),
            PaletteCase(
                TerminalThemeChoice.SOLARIZED_LIGHT,
                "#FDF6E3",
                "#657B83",
                "#073642",
                "#073642",
            ),
        )

        assertEquals(TerminalThemeChoice.entries.toList(), cases.map(PaletteCase::choice))
        cases.forEach { case ->
            val visuals = terminalPaletteVisuals(
                backgroundHex = case.background,
                foregroundHex = case.foreground,
                ansiBlackHex = case.ansiBlack,
            )
            assertEquals(
                color(case.background),
                visuals.background,
            )
            assertEquals(
                color(case.foreground),
                visuals.fallbackForeground,
            )
            assertEquals(
                color(case.statusForeground),
                visuals.statusForeground,
            )
        }
    }

    @Test
    fun terminalChromeUsesCanonicalColorsIndependentOfAppTheme() {
        assertEquals(color("#02082C"), TerminalVisualDefaults.chromeBackground)
        assertEquals(color("#EAFBFF"), TerminalVisualDefaults.chromeForeground)
        assertEquals(color("#0DD5F0"), TerminalVisualDefaults.chromeAccent)
        assertEquals(color("#FF5C57"), TerminalVisualDefaults.chromeDanger)
        assertEquals(color("#F3F99D"), TerminalVisualDefaults.chromeWarning)
        assertEquals(color("#011B44"), TerminalVisualDefaults.chromeSurface)
        assertEquals(color("#A8DCEB"), TerminalVisualDefaults.chromeSecondary)
        assertEquals(color("#83AFC2"), TerminalVisualDefaults.chromeMuted)
        assertEquals(TerminalVisualDefaults.chromeBackground, TerminalVisualDefaults.chromeOnAccent)
    }

    @Test
    fun terminalPhaseStatusesUseCanonicalChromeRoles() {
        assertEquals(
            TerminalVisualDefaults.chromeForeground,
            phaseColor(TerminalSessionController.Phase.IDLE),
        )
        assertEquals(
            TerminalVisualDefaults.chromeWarning,
            phaseColor(TerminalSessionController.Phase.CONNECTING),
        )
        assertEquals(
            TerminalVisualDefaults.chromeAccent,
            phaseColor(TerminalSessionController.Phase.RUNNING),
        )
        assertEquals(
            TerminalVisualDefaults.chromeForeground,
            phaseColor(TerminalSessionController.Phase.EXITED),
        )
        assertEquals(
            TerminalVisualDefaults.chromeDanger,
            phaseColor(TerminalSessionController.Phase.FAILED),
        )
    }

    @Test
    fun alternateThemeMappingsRemainStable() {
        assertEquals(
            TerminalThemePreset.CatppuccinFrappe,
            TerminalThemeChoice.CATPPUCCIN_FRAPPE.toPreset(),
        )
        assertEquals(
            TerminalThemePreset.CatppuccinFrappeLight,
            TerminalThemeChoice.CATPPUCCIN_FRAPPE_LIGHT.toPreset(),
        )
        assertEquals(
            TerminalThemePreset.Solarized(dark = true),
            TerminalThemeChoice.SOLARIZED_DARK.toPreset(),
        )
        assertEquals(
            TerminalThemePreset.Solarized(dark = false),
            TerminalThemeChoice.SOLARIZED_LIGHT.toPreset(),
        )
    }

    private fun config(theme: TerminalThemePreset) = TerminalConfig(
        theme = theme,
        fontFamily = TerminalVisualDefaults.nativeGridFontFamily,
        fontSizePt = 13f,
        cursorStyle = TerminalCursorStyle.BAR,
        cursorBlink = true,
        scrollbackLines = 10_000u,
    )

    private class RecordingConfigTarget : TerminalConfigTarget {
        val applied = mutableListOf<TerminalConfig>()

        override fun apply(config: TerminalConfig) {
            applied += config
        }
    }

    private data class PaletteCase(
        val choice: TerminalThemeChoice,
        val background: String,
        val foreground: String,
        val ansiBlack: String,
        val statusForeground: String,
    )

    private fun color(hex: String): Color =
        Color(0xFF000000L or hex.removePrefix("#").toLong(16))
}
