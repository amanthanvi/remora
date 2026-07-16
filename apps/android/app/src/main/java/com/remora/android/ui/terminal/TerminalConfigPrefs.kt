package com.remora.android.ui.terminal

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import uniffi.codex_mobile_client.TerminalConfig
import uniffi.codex_mobile_client.TerminalCursorStyle
import uniffi.codex_mobile_client.TerminalThemePreset
import uniffi.codex_mobile_client.themePalette

internal data class TerminalPaletteVisuals(
    val background: Color,
    val fallbackForeground: Color,
    val statusForeground: Color,
)

internal object TerminalVisualDefaults {
    /** Terminal chrome intentionally stays on Remora's canonical dark canvas. */
    val chromeTheme = TerminalThemeChoice.REMORA_DARK

    val chromeBackground = Color(0xFF02082C)
    val chromeForeground = Color(0xFFEAFBFF)
    val chromeAccent = Color(0xFF0DD5F0)
    val chromeDanger = Color(0xFFFF5C57)
    val chromeWarning = Color(0xFFF3F99D)

    // These are the opaque secondary surface/text roles from the canonical
    // Remora dark theme. Keeping them here prevents terminal chrome from
    // changing contrast when the rest of the app uses a light/custom theme.
    val chromeSurface = Color(0xFF011B44)
    val chromeSecondary = Color(0xFFA8DCEB)
    val chromeMuted = Color(0xFF83AFC2)
    val chromeOnAccent: Color
        get() = chromeBackground

    /**
     * Ghostty's Android font backend discovers installed fonts through
     * fontconfig; Android resource fonts are not registered there. Compose
     * chrome uses the bundled Berkeley Mono through `RemoraTheme.monoFont`,
     * while the native grid requests Android's guaranteed monospace family.
     * Never claim Apple's SF Mono on Android.
     */
    const val nativeGridFontFamily = "monospace"
}

private fun parseTerminalColor(hex: String): Color {
    require(hex.length == 7 && hex[0] == '#') { "expected #RRGGBB terminal color" }
    return Color(0xFF000000L or hex.substring(1).toLong(16))
}

/**
 * Compose-side colors for the selected Ghostty palette. Plain-text fallback
 * output keeps the preset foreground. Lightweight status copy uses ANSI black
 * on light canvases, matching the shared Rust contrast contract.
 */
internal fun terminalPaletteVisuals(theme: TerminalThemePreset): TerminalPaletteVisuals {
    val palette = themePalette(theme)
    return terminalPaletteVisuals(
        backgroundHex = palette.background,
        foregroundHex = palette.foreground,
        ansiBlackHex = palette.ansi.firstOrNull(),
    )
}

internal fun terminalPaletteVisuals(
    backgroundHex: String,
    foregroundHex: String,
    ansiBlackHex: String?,
): TerminalPaletteVisuals {
    val background = parseTerminalColor(backgroundHex)
    val fallbackForeground = parseTerminalColor(foregroundHex)
    val statusForeground = if (background.luminance() > 0.5f) {
        parseTerminalColor(ansiBlackHex ?: foregroundHex)
    } else {
        fallbackForeground
    }
    return TerminalPaletteVisuals(
        background = background,
        fallbackForeground = fallbackForeground,
        statusForeground = statusForeground,
    )
}

internal fun terminalCanvasColor(theme: TerminalThemePreset): Color =
    terminalPaletteVisuals(theme).background

enum class TerminalThemeChoice(val id: String, val title: String) {
    REMORA_DARK("remora-dark", "Remora Dark"),
    CATPPUCCIN_FRAPPE("catppuccin-frappe", "Catppuccin Frappé"),
    CATPPUCCIN_FRAPPE_LIGHT("catppuccin-frappe-light", "Catppuccin Frappé Light"),
    SOLARIZED_DARK("solarized-dark", "Solarized Dark"),
    SOLARIZED_LIGHT("solarized-light", "Solarized Light");

    fun toPreset(): TerminalThemePreset = when (this) {
        REMORA_DARK -> TerminalThemePreset.RemoraDark
        CATPPUCCIN_FRAPPE -> TerminalThemePreset.CatppuccinFrappe
        CATPPUCCIN_FRAPPE_LIGHT -> TerminalThemePreset.CatppuccinFrappeLight
        SOLARIZED_DARK -> TerminalThemePreset.Solarized(dark = true)
        SOLARIZED_LIGHT -> TerminalThemePreset.Solarized(dark = false)
    }

    companion object {
        val DEFAULT = REMORA_DARK

        fun fromId(id: String?): TerminalThemeChoice =
            entries.firstOrNull { it.id == id } ?: DEFAULT
    }
}

/**
 * Persisted terminal config (font size, theme, cursor blink). Mirrors the
 * iOS `@AppStorage` keys so the two platforms feel identical.
 */
object TerminalConfigPrefs {
    private const val PREFS = "remora_terminal_prefs"
    private const val KEY_FONT_SIZE = "fontSize"
    private const val KEY_THEME_ID = "themeId"
    private const val KEY_CURSOR_BLINK = "cursorBlink"

    private const val DEFAULT_FONT_SIZE = 13.0f
    private const val DEFAULT_CURSOR_BLINK = true

    var fontSize by mutableFloatStateOf(DEFAULT_FONT_SIZE)
        private set
    var theme by mutableStateOf(TerminalThemeChoice.DEFAULT)
        private set
    var cursorBlink by mutableStateOf(DEFAULT_CURSOR_BLINK)
        private set

    fun initialize(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        fontSize = prefs.getFloat(KEY_FONT_SIZE, DEFAULT_FONT_SIZE)
        theme = TerminalThemeChoice.fromId(prefs.getString(KEY_THEME_ID, TerminalThemeChoice.DEFAULT.id))
        cursorBlink = prefs.getBoolean(KEY_CURSOR_BLINK, DEFAULT_CURSOR_BLINK)
    }

    fun setFontSize(context: Context, value: Float) {
        val clamped = value.coerceIn(10f, 24f)
        fontSize = clamped
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putFloat(KEY_FONT_SIZE, clamped).apply()
    }

    fun setTheme(context: Context, choice: TerminalThemeChoice) {
        theme = choice
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(KEY_THEME_ID, choice.id).apply()
    }

    fun setCursorBlink(context: Context, enabled: Boolean) {
        cursorBlink = enabled
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putBoolean(KEY_CURSOR_BLINK, enabled).apply()
    }

    fun currentConfig(): TerminalConfig = TerminalConfig(
        theme = theme.toPreset(),
        fontFamily = TerminalVisualDefaults.nativeGridFontFamily,
        fontSizePt = fontSize,
        cursorStyle = TerminalCursorStyle.BAR,
        cursorBlink = cursorBlink,
        scrollbackLines = 10_000u,
    )
}
