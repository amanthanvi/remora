package com.remora.android.ui

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoraAppearanceModeTest {
    @Test
    fun parsesStoredAppearanceModes() {
        assertEquals(RemoraAppearanceMode.SYSTEM, RemoraAppearanceMode.fromStorageValue("system"))
        assertEquals(RemoraAppearanceMode.LIGHT, RemoraAppearanceMode.fromStorageValue("LIGHT"))
        assertEquals(RemoraAppearanceMode.DARK, RemoraAppearanceMode.fromStorageValue("dark"))
    }

    @Test
    fun ignoresUnknownStoredAppearanceMode() {
        assertNull(RemoraAppearanceMode.fromStorageValue("sepia"))
        assertNull(RemoraAppearanceMode.fromStorageValue(null))
    }

    @Test
    fun resolvesDarkThemeFromSystemPreference() {
        assertEquals(false, RemoraAppearanceMode.SYSTEM.resolvesDarkTheme(systemIsDark = false))
        assertEquals(true, RemoraAppearanceMode.SYSTEM.resolvesDarkTheme(systemIsDark = true))
        assertEquals(false, RemoraAppearanceMode.LIGHT.resolvesDarkTheme(systemIsDark = true))
        assertEquals(true, RemoraAppearanceMode.DARK.resolvesDarkTheme(systemIsDark = false))
    }

    @Test
    fun defaultThemesUseRemoraIdentity() {
        assertEquals("remora-dark", RemoraResolvedTheme.defaultDark.slug)
        assertEquals("Remora", RemoraResolvedTheme.defaultDark.name)
        assertEquals(RemoraColorThemeType.DARK, RemoraResolvedTheme.defaultDark.type)

        assertEquals("remora-light", RemoraResolvedTheme.defaultLight.slug)
        assertEquals("Remora Light", RemoraResolvedTheme.defaultLight.name)
        assertEquals(RemoraColorThemeType.LIGHT, RemoraResolvedTheme.defaultLight.type)
    }

    @Test
    fun parsesSupportedHexColorFormsWithoutAndroidRuntime() {
        assertColorEquals(Color(0xFFAABBCC), colorFromHex("#ABC"))
        assertColorEquals(Color(0xDDAABBCC), colorFromHex("#DABC"))
        assertColorEquals(Color(0xFFAABBCC), colorFromHex("#AABBCC"))
        assertColorEquals(Color(0xDDAABBCC), colorFromHex("#DDAABBCC"))

        val fallback = Color.Magenta
        assertColorEquals(fallback, colorFromHex("not-a-color", fallback))
        assertColorEquals(fallback, colorFromHex("AABBCC", fallback))
        assertColorEquals(fallback, colorFromHex("#-0000000", fallback))
        assertColorEquals(fallback, colorFromHex("#12", fallback))
        assertColorEquals(fallback, colorFromHex(null, fallback))
        assertColorEquals(fallback, colorFromHex("   ", fallback))
    }

    @Test
    fun defaultReadableRolesStayOpaqueAndMeetAaAgainstEveryAppSurface() {
        listOf(RemoraResolvedTheme.defaultDark, RemoraResolvedTheme.defaultLight).forEach { theme ->
            val readableRoles =
                mapOf(
                    "primary" to theme.textPrimary,
                    "secondary" to theme.textSecondary,
                    "muted" to theme.textMuted,
                    "accent" to theme.accent,
                )
            readableRoles.forEach { (role, color) ->
                assertEquals("${theme.slug} $role alpha", 1f, color.alpha, 0f)
                listOf(theme.background, theme.surface, theme.surfaceLight).forEach { surface ->
                    val ratio = contrastRatio(color, surface)
                    assertTrue(
                        "${theme.slug} $role contrast was $ratio",
                        ratio >= 4.5,
                    )
                }
            }

            val onAccentRatio = contrastRatio(theme.textOnAccent, theme.accentStrong)
            assertTrue(
                "${theme.slug} on-accent contrast was $onAccentRatio",
                onAccentRatio >= 4.5,
            )
        }
    }

    private fun contrastRatio(
        foreground: Color,
        background: Color,
    ): Double {
        val foregroundLuminance = foreground.relativeLuminance()
        val backgroundLuminance = background.relativeLuminance()
        return (maxOf(foregroundLuminance, backgroundLuminance) + 0.05) /
            (minOf(foregroundLuminance, backgroundLuminance) + 0.05)
    }

    private fun Color.relativeLuminance(): Double =
        listOf(red, green, blue)
            .map { component ->
                val value = component.toDouble()
                if (value <= 0.04045) value / 12.92 else Math.pow((value + 0.055) / 1.055, 2.4)
            }
            .let { (red, green, blue) -> (0.2126 * red) + (0.7152 * green) + (0.0722 * blue) }

    private fun assertColorEquals(
        expected: Color,
        actual: Color,
    ) {
        assertEquals(expected.red, actual.red, 0.0001f)
        assertEquals(expected.green, actual.green, 0.0001f)
        assertEquals(expected.blue, actual.blue, 0.0001f)
        assertEquals(expected.alpha, actual.alpha, 0.0001f)
    }
}
