package com.remora.android.state

import android.annotation.SuppressLint
import android.content.Context
import java.io.File

/**
 * One-time greenfield product-state cutover. Only obsolete local cache and UI
 * preferences are removed. Credentials, pairing keys, Host trust, and Remora
 * Link identity use separate stores and are deliberately untouched.
 */
internal object CurrentWorkspaceRebuild {
    internal const val MARKER_PREFS = "remora_workspace_rebuild"
    internal const val MARKER_KEY = "completed_2"

    data class Result(val completed: Boolean, val didRebuild: Boolean)

    fun apply(context: Context): Result = apply(AndroidWorkspaceRebuildBackend(context.applicationContext))

    internal fun apply(backend: WorkspaceRebuildBackend): Result {
        if (backend.isComplete()) return Result(completed = true, didRebuild = false)
        if (!backend.clearObsoleteProductState()) return Result(completed = false, didRebuild = false)
        if (!backend.removeRetiredFeatureOverrides()) return Result(completed = false, didRebuild = false)
        if (!backend.markComplete() || !backend.isComplete()) {
            return Result(completed = false, didRebuild = false)
        }
        return Result(completed = true, didRebuild = true)
    }
}

internal interface WorkspaceRebuildBackend {
    fun isComplete(): Boolean
    fun clearObsoleteProductState(): Boolean
    fun removeRetiredFeatureOverrides(): Boolean
    fun markComplete(): Boolean
}

private class AndroidWorkspaceRebuildBackend(
    private val context: Context,
) : WorkspaceRebuildBackend {
    private val marker
        get() = context.getSharedPreferences(CurrentWorkspaceRebuild.MARKER_PREFS, Context.MODE_PRIVATE)

    override fun isComplete(): Boolean =
        marker.getBoolean(CurrentWorkspaceRebuild.MARKER_KEY, false)

    override fun clearObsoleteProductState(): Boolean {
        val obsoletePaths = listOf(
            File(context.filesDir, "Apps"),
            File(context.filesDir, "RemoraPreferences/mobile_prefs.json"),
            File(context.filesDir, "RemoraPreferences/mobile_prefs.json.tmp"),
        )
        return obsoletePaths.all(::deleteTree)
    }

    @SuppressLint("ApplySharedPref")
    override fun removeRetiredFeatureOverrides(): Boolean {
        val featurePreferences = context.getSharedPreferences("remora_ui_prefs", Context.MODE_PRIVATE)
        val features = featurePreferences
            .getStringSet("remora.experimentalFeatures", emptySet())
            .orEmpty()
            .filterNot { entry -> entry.substringBefore('=') == RETIRED_MINIGAME_FEATURE }
            .toSet()
        return featurePreferences.edit().putStringSet("remora.experimentalFeatures", features).commit()
    }

    private fun deleteTree(file: File): Boolean = !file.exists() || file.deleteRecursively()

    @SuppressLint("ApplySharedPref")
    override fun markComplete(): Boolean =
        marker.edit().putBoolean(CurrentWorkspaceRebuild.MARKER_KEY, true).commit()

    private companion object {
        const val RETIRED_MINIGAME_FEATURE = "thinking_minigame"
    }
}
