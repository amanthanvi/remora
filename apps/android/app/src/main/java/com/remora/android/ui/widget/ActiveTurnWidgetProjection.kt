package com.remora.android.ui.widget

internal data class ActiveTurnWidgetProjection(
    val isActive: Boolean,
    val countLabel: String,
    val statusLabel: String,
)

internal fun activeTurnWidgetProjection(activeCount: Int): ActiveTurnWidgetProjection {
    val normalizedCount = activeCount.coerceAtLeast(0)
    val isActive = normalizedCount > 0
    val countLabel = when {
        normalizedCount == 0 -> "No active turns"
        normalizedCount == 1 -> "1 active turn"
        normalizedCount > 99 -> "99+ active turns"
        else -> "$normalizedCount active turns"
    }

    return ActiveTurnWidgetProjection(
        isActive = isActive,
        countLabel = countLabel,
        statusLabel = if (isActive) "Running" else "Idle",
    )
}
