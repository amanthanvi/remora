package com.remora.android.ui

import android.content.Context
import android.content.res.Configuration
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import org.json.JSONArray
import org.json.JSONObject

private const val THEME_LOG_TAG = "RemoraThemeManager"
private const val UI_PREFERENCES_NAME = "remora_ui_prefs"
private const val SELECTED_LIGHT_THEME_KEY = "selected_light_theme"
private const val SELECTED_DARK_THEME_KEY = "selected_dark_theme"
private const val APPEARANCE_MODE_KEY = "appearance_mode"
private const val DARK_MODE_KEY = "dark_mode_enabled"
private const val FONT_MONO_KEY = "font_family_mono"

enum class RemoraAppearanceMode(
    val storageValue: String,
    val displayName: String,
) {
    SYSTEM("system", "System"),
    LIGHT("light", "Light"),
    DARK("dark", "Dark");

    companion object {
        fun fromStorageValue(value: String?): RemoraAppearanceMode? =
            entries.firstOrNull { it.storageValue.equals(value, ignoreCase = true) }
    }

    fun resolvesDarkTheme(systemIsDark: Boolean): Boolean =
        when (this) {
            SYSTEM -> systemIsDark
            LIGHT -> false
            DARK -> true
        }
}

enum class RemoraColorThemeType {
    LIGHT,
    DARK,
}

data class RemoraThemeIndexEntry(
    val slug: String,
    val name: String,
    val type: RemoraColorThemeType,
    val accentHex: String,
    val backgroundHex: String,
    val foregroundHex: String,
)

data class RemoraThemeDefinition(
    val name: String,
    val type: RemoraColorThemeType,
    val colors: Map<String, String>,
)

data class RemoraResolvedTheme(
    val slug: String,
    val name: String,
    val type: RemoraColorThemeType,
    val background: Color,
    val surface: Color,
    val surfaceLight: Color,
    val textPrimary: Color,
    val textSecondary: Color,
    val textMuted: Color,
    val textBody: Color,
    val textSystem: Color,
    val accent: Color,
    val accentStrong: Color,
    val border: Color,
    val separator: Color,
    val danger: Color,
    val success: Color,
    val warning: Color,
    val textOnAccent: Color,
    val codeBackground: Color,
) {
    companion object {
        val defaultLight =
            resolve(
                slug = "remora-light",
                definition =
                    RemoraThemeDefinition(
                        name = "Remora Light",
                        type = RemoraColorThemeType.LIGHT,
                        colors =
                            mapOf(
                                "editor.background" to "#F7FCFE",
                                "editor.foreground" to "#102A36",
                                "sideBar.background" to "#EAF7FB",
                                "sideBar.foreground" to "#365866",
                                "activityBar.background" to "#DCEFF5",
                                "editorLineNumber.foreground" to "#4E6671",
                                "editorGroup.border" to "#B7DCE7",
                                "panel.border" to "#D8E8F1",
                                "textLink.foreground" to "#036F8F",
                                "button.background" to "#036F8F",
                            ),
                    ),
            )

        val defaultDark =
            resolve(
                slug = "remora-dark",
                definition =
                    RemoraThemeDefinition(
                        name = "Remora",
                        type = RemoraColorThemeType.DARK,
                        colors =
                            mapOf(
                                "editor.background" to "#02082C",
                                "editor.foreground" to "#EAFBFF",
                                "sideBar.background" to "#011B44",
                                "sideBar.foreground" to "#A8DCEB",
                                "activityBar.background" to "#022753",
                                "editorLineNumber.foreground" to "#83AFC2",
                                "editorGroup.border" to "#044875",
                                "panel.border" to "#02356A",
                                "textLink.foreground" to "#0DD5F0",
                                "button.background" to "#07F2FB",
                            ),
                    ),
            )

        fun resolve(
            slug: String,
            definition: RemoraThemeDefinition,
        ): RemoraResolvedTheme {
            val colors = definition.colors
            val background =
                colorFromHex(
                    colors["editor.background"],
                    fallback = if (definition.type == RemoraColorThemeType.DARK) Color(0xFF111111) else Color.White,
                )
            val foreground =
                colorFromHex(
                    colors["editor.foreground"],
                    fallback = if (definition.type == RemoraColorThemeType.DARK) Color(0xFFFCFCFC) else Color(0xFF0D0D0D),
                )
            val surface =
                colors["sideBar.background"]?.let(::colorFromHex)
                    ?: adjustBrightness(background, if (definition.type == RemoraColorThemeType.DARK) 0.03f else -0.02f)
            val surfaceLight =
                colors["activityBar.background"]?.let(::colorFromHex)
                    ?: adjustBrightness(surface, if (definition.type == RemoraColorThemeType.DARK) 0.04f else -0.03f)
            val accent =
                colors["textLink.foreground"]?.let(::colorFromHex)
                    ?: colors["button.background"]?.let(::colorFromHex)
                    ?: if (definition.type == RemoraColorThemeType.DARK) Color(0xFFB0B0B0) else Color(0xFF4A4A4A)
            val accentStrong =
                colors["button.background"]?.let(::colorFromHex)
                    ?: colors["textLink.foreground"]?.let(::colorFromHex)
                    ?: accent
            val border =
                colors["editorGroup.border"]?.let(::colorFromHex)
                    ?: colors["sideBar.border"]?.let(::colorFromHex)
                    ?: adjustBrightness(surface, if (definition.type == RemoraColorThemeType.DARK) 0.05f else -0.05f)
            val separator =
                colors["panel.border"]?.let(::colorFromHex)
                    ?: adjustBrightness(background, if (definition.type == RemoraColorThemeType.DARK) 0.04f else -0.04f)

            return RemoraResolvedTheme(
                slug = slug,
                name = definition.name,
                type = definition.type,
                background = background,
                surface = surface,
                surfaceLight = surfaceLight,
                textPrimary = foreground,
                textSecondary = colors["sideBar.foreground"]?.let(::colorFromHex) ?: dimColor(foreground, 0.55f),
                textMuted = colors["editorLineNumber.foreground"]?.let(::colorFromHex) ?: dimColor(foreground, 0.35f),
                textBody = dimColor(foreground, 0.88f),
                textSystem = dimColor(foreground, 0.7f),
                accent = accent,
                accentStrong = accentStrong,
                border = border,
                separator = separator,
                danger = if (definition.type == RemoraColorThemeType.DARK) Color(0xFFFF5555) else Color(0xFFD32F2F),
                success = if (definition.type == RemoraColorThemeType.DARK) Color(0xFF6EA676) else Color(0xFF2E7D32),
                warning = if (definition.type == RemoraColorThemeType.DARK) Color(0xFFE2A644) else Color(0xFFA84400),
                textOnAccent = if (brightness(accentStrong) > 0.5f) Color(0xFF0D0D0D) else Color.White,
                codeBackground = background,
            )
        }

        fun brightness(color: Color): Float = (0.299f * color.red) + (0.587f * color.green) + (0.114f * color.blue)

        fun adjustBrightness(
            color: Color,
            amount: Float,
        ): Color =
            Color(
                red = (color.red + amount).coerceIn(0f, 1f),
                green = (color.green + amount).coerceIn(0f, 1f),
                blue = (color.blue + amount).coerceIn(0f, 1f),
                alpha = color.alpha,
            )

        fun dimColor(
            color: Color,
            factor: Float,
        ): Color =
            if (brightness(color) > 0.5f) {
                Color(
                    red = (color.red * factor).coerceIn(0f, 1f),
                    green = (color.green * factor).coerceIn(0f, 1f),
                    blue = (color.blue * factor).coerceIn(0f, 1f),
                    alpha = color.alpha,
                )
            } else {
                val inverse = 1f - factor
                Color(
                    red = (color.red + ((1f - color.red) * inverse)).coerceIn(0f, 1f),
                    green = (color.green + ((1f - color.green) * inverse)).coerceIn(0f, 1f),
                    blue = (color.blue + ((1f - color.blue) * inverse)).coerceIn(0f, 1f),
                    alpha = color.alpha,
                )
            }
    }
}

internal fun colorFromHex(
    hex: String?,
    fallback: Color = Color.Transparent,
): Color {
    val normalized = hex?.trim()?.takeIf { it.isNotEmpty() } ?: return fallback
    if (!normalized.startsWith("#")) return fallback
    val digits = normalized.drop(1)
    if (digits.any { it !in '0'..'9' && it.lowercaseChar() !in 'a'..'f' }) return fallback
    val argb =
        when (digits.length) {
            3 -> "FF" + digits.flatMap { listOf(it, it) }.joinToString("")
            4 -> digits.flatMap { listOf(it, it) }.joinToString("")
            6 -> "FF$digits"
            8 -> digits
            else -> return fallback
        }
    return runCatching {
        val packed = argb.toLong(radix = 16)
        Color(
            red = ((packed shr 16) and 0xFF).toFloat() / 255f,
            green = ((packed shr 8) and 0xFF).toFloat() / 255f,
            blue = (packed and 0xFF).toFloat() / 255f,
            alpha = ((packed shr 24) and 0xFF).toFloat() / 255f,
        )
    }.getOrElse { fallback }
}

object RemoraThemeManager {
    private val lock = Any()
    private var appContext: Context? = null
    private var initialized = false
    private var definitionCache = LinkedHashMap<String, RemoraThemeDefinition>()
    private var systemIsDark = false

    var appearanceMode by mutableStateOf(RemoraAppearanceMode.SYSTEM)
        private set

    var monoFontEnabled by mutableStateOf(true)
        private set

    var lightTheme by mutableStateOf(RemoraResolvedTheme.defaultLight)
        private set

    var darkTheme by mutableStateOf(RemoraResolvedTheme.defaultDark)
        private set

    var activeTheme by mutableStateOf(RemoraResolvedTheme.defaultDark)
        private set

    var themeVersion by mutableIntStateOf(0)
        private set

    var themeIndex by mutableStateOf<List<RemoraThemeIndexEntry>>(emptyList())
        private set

    val lightThemes: List<RemoraThemeIndexEntry>
        get() = themeIndex.filter { it.type == RemoraColorThemeType.LIGHT }

    val darkThemes: List<RemoraThemeIndexEntry>
        get() = themeIndex.filter { it.type == RemoraColorThemeType.DARK }

    val selectedLightSlug: String
        get() = preferences?.getString(SELECTED_LIGHT_THEME_KEY, null) ?: "remora-light"

    val selectedDarkSlug: String
        get() = preferences?.getString(SELECTED_DARK_THEME_KEY, null) ?: "remora-dark"

    private val preferences
        get() = appContext?.getSharedPreferences(UI_PREFERENCES_NAME, Context.MODE_PRIVATE)

    fun initialize(context: Context) {
        synchronized(lock) {
            if (initialized) {
                return
            }
            appContext = context.applicationContext
            themeIndex = loadThemeIndex()
            lightTheme = loadAndResolve(selectedLightSlug) ?: RemoraResolvedTheme.defaultLight
            darkTheme = loadAndResolve(selectedDarkSlug) ?: RemoraResolvedTheme.defaultDark
            val nightModeFlags = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK
            val systemIsDarkMode = nightModeFlags == Configuration.UI_MODE_NIGHT_YES
            systemIsDark = systemIsDarkMode
            appearanceMode = loadAppearanceMode()
            activeTheme = themeForMode(appearanceMode)
            monoFontEnabled = preferences?.getBoolean(FONT_MONO_KEY, true) ?: true
            initialized = true
        }
    }

    fun applySystemTheme(isDark: Boolean) {
        systemIsDark = isDark
        applyActiveTheme()
    }

    fun applyDarkMode(enabled: Boolean) {
        applyAppearanceMode(if (enabled) RemoraAppearanceMode.DARK else RemoraAppearanceMode.LIGHT)
    }

    fun applyAppearanceMode(mode: RemoraAppearanceMode) {
        preferences?.edit()?.putString(APPEARANCE_MODE_KEY, mode.storageValue)?.apply()
        if (appearanceMode != mode) {
            appearanceMode = mode
            themeVersion += 1
        }
        applyActiveTheme()
    }

    fun applyFont(isMono: Boolean) {
        preferences?.edit()?.putBoolean(FONT_MONO_KEY, isMono)?.apply()
        monoFontEnabled = isMono
    }

    fun selectLightTheme(slug: String) {
        preferences?.edit()?.putString(SELECTED_LIGHT_THEME_KEY, slug)?.apply()
        lightTheme = loadAndResolve(slug) ?: RemoraResolvedTheme.defaultLight
        if (!usesDarkTheme()) {
            activeTheme = lightTheme
        }
        themeVersion += 1
    }

    fun selectDarkTheme(slug: String) {
        preferences?.edit()?.putString(SELECTED_DARK_THEME_KEY, slug)?.apply()
        darkTheme = loadAndResolve(slug) ?: RemoraResolvedTheme.defaultDark
        if (usesDarkTheme()) {
            activeTheme = darkTheme
        }
        themeVersion += 1
    }

    private fun loadAppearanceMode(): RemoraAppearanceMode {
        val prefs = preferences ?: return RemoraAppearanceMode.SYSTEM
        RemoraAppearanceMode.fromStorageValue(prefs.getString(APPEARANCE_MODE_KEY, null))?.let {
            return it
        }
        return if (prefs.contains(DARK_MODE_KEY)) {
            if (prefs.getBoolean(DARK_MODE_KEY, false)) {
                RemoraAppearanceMode.DARK
            } else {
                RemoraAppearanceMode.LIGHT
            }
        } else {
            RemoraAppearanceMode.SYSTEM
        }
    }

    private fun usesDarkTheme(mode: RemoraAppearanceMode = appearanceMode): Boolean =
        mode.resolvesDarkTheme(systemIsDark)

    private fun themeForMode(mode: RemoraAppearanceMode): RemoraResolvedTheme =
        if (usesDarkTheme(mode)) {
            darkTheme
        } else {
            lightTheme
        }

    private fun applyActiveTheme() {
        val nextTheme = themeForMode(appearanceMode)
        if (activeTheme.slug != nextTheme.slug || activeTheme.type != nextTheme.type) {
            activeTheme = nextTheme
        }
    }

    private fun loadThemeIndex(): List<RemoraThemeIndexEntry> {
        val context = appContext ?: return emptyList()
        return runCatching {
            context.assets.open("theme-manifest.json").bufferedReader().use { reader ->
                val array = JSONArray(reader.readText())
                buildList(array.length()) {
                    for (index in 0 until array.length()) {
                        val item = array.getJSONObject(index)
                        add(
                            RemoraThemeIndexEntry(
                                slug = item.optString("slug"),
                                name = item.optString("name"),
                                type = item.optString("type").toThemeType(),
                                accentHex = item.optString("accentHex"),
                                backgroundHex = item.optString("backgroundHex"),
                                foregroundHex = item.optString("foregroundHex"),
                            ),
                        )
                    }
                }
            }
        }.onFailure { error ->
            Log.w(THEME_LOG_TAG, "Failed to load theme manifest", error)
        }.getOrDefault(emptyList())
    }

    private fun loadAndResolve(slug: String): RemoraResolvedTheme? {
        val definition = loadDefinition(slug) ?: return null
        return RemoraResolvedTheme.resolve(slug = slug, definition = definition)
    }

    private fun loadDefinition(slug: String): RemoraThemeDefinition? {
        definitionCache[slug]?.let { return it }
        val context = appContext ?: return null
        return runCatching {
            context.assets.open("$slug.json").bufferedReader().use { reader ->
                parseThemeDefinition(JSONObject(reader.readText())).also { parsed ->
                    definitionCache[slug] = parsed
                }
            }
        }.onFailure { error ->
            Log.w(THEME_LOG_TAG, "Failed to load theme $slug", error)
        }.getOrNull()
    }

    private fun parseThemeDefinition(json: JSONObject): RemoraThemeDefinition {
        val colorsJson = json.optJSONObject("colors") ?: JSONObject()
        val colors = LinkedHashMap<String, String>()
        val keys = colorsJson.keys()
        while (keys.hasNext()) {
            val key = keys.next()
            colors[key] = colorsJson.optString(key)
        }
        return RemoraThemeDefinition(
            name = json.optString("name"),
            type = json.optString("type").toThemeType(),
            colors = colors,
        )
    }
}

private fun String.toThemeType(): RemoraColorThemeType =
    if (equals("light", ignoreCase = true)) {
        RemoraColorThemeType.LIGHT
    } else {
        RemoraColorThemeType.DARK
    }
