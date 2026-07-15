package com.remora.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
}
