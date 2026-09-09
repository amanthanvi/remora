package com.remora.android.state

import uniffi.codex_mobile_client.ThreadKey

internal data class AppModelActivationRequest(val id: Long, val key: ThreadKey?)

/** Accessed only under AppModel's projection guard. */
internal class AppModelNavigationIntent {
    private var nextId = 0L
    private var pending: AppModelActivationRequest? = null
    private var completionFence = 0L

    fun request(key: ThreadKey?): AppModelActivationRequest =
        AppModelActivationRequest(++nextId, key).also { pending = it }

    fun project(authoritative: ThreadKey?): ThreadKey? {
        val request = pending
        return if (request != null) request.key else authoritative
    }

    fun beginDispatch(request: AppModelActivationRequest): Boolean = pending?.id == request.id

    fun completed(request: AppModelActivationRequest) {
        completionFence = maxOf(completionFence, request.id)
    }

    fun snapshotFence(): Long = completionFence

    fun projectSnapshot(authoritative: ThreadKey?, fence: Long): ThreadKey? {
        // Subscription updates can be coalesced or lost. Only a canonical read
        // started after this setter completed releases its optimistic selection.
        val request = pending
        if (request != null && request.id <= fence) {
            pending = null
        }
        return project(authoritative)
    }

    fun failed(request: AppModelActivationRequest) {
        if (pending?.id == request.id) pending = null
    }

    fun removed(key: ThreadKey) {
        if (pending?.key == key) pending = null
    }
}
