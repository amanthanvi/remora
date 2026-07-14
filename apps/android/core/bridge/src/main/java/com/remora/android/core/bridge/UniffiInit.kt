package com.remora.android.core.bridge

import android.content.Context
import java.io.File

/**
 * Initializes the UniFFI bindings and platform environment.
 *
 * - Redirects UniFFI/JNA to load `codex_mobile_client` as the Android native library
 * - Sets HOME and CODEX_HOME so Rust can find/create its data directories
 *
 * Must be called before any UniFFI-generated class is instantiated.
 */
object UniffiInit {
    private var initialized = false

    @Synchronized
    fun ensure(context: Context? = null) {
        // Set JNA library override
        System.setProperty(
            "uniffi.component.codex_mobile_client.libraryOverride",
            "codex_mobile_client",
        )

        if (initialized) return

        val appContext = context?.applicationContext
        if (appContext == null) {
            android.util.Log.w("UniffiInit", "Native init deferred: Android context missing")
            return
        }

        // Set HOME and CODEX_HOME for Rust (Android doesn't set HOME by default)
        val filesDir = appContext.filesDir.absolutePath
        val codexHome = File(appContext.filesDir, "codex-home")
        codexHome.mkdirs()

        // Keep Java APIs aligned with the native HOME configured below.
        System.setProperty("user.home", filesDir)

        try {
            System.loadLibrary("codex_mobile_client")
            check(nativeMobileClientInit(appContext, filesDir, codexHome.absolutePath)) {
                "Native environment initialization failed"
            }
            android.util.Log.i("UniffiInit", "Native init complete")
        } catch (e: Throwable) {
            android.util.Log.e("UniffiInit", "Native init failed", e)
            throw e
        }

        initialized = true
    }

    @JvmStatic
    private external fun nativeMobileClientInit(
        context: Context,
        homeDir: String,
        codexHomeDir: String,
    ): Boolean

    @JvmStatic
    fun debugNativeContextProbe(context: Context): String {
        ensure(context.applicationContext)
        return nativeMobileClientContextProbe()
    }

    @JvmStatic
    private external fun nativeMobileClientContextProbe(): String
}
