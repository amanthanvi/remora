package com.remora.android.state

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppStartThreadRequest
import uniffi.codex_mobile_client.ThreadKey

sealed interface ComposerDraftDestination {
    data class Conversation(val key: ThreadKey) : ComposerDraftDestination
    data class Home(val serverId: String?, val cwd: String?) : ComposerDraftDestination
}

enum class ComposerDraftRecoveryStatus { SUBMITTING, UNCONFIRMED, SAVED }

data class RecoverableComposerDraft(
    val id: Long,
    val destination: ComposerDraftDestination,
    val draft: AppModel.ComposerDraft,
    val status: ComposerDraftRecoveryStatus,
    val payload: AppComposerPayload? = null,
    val threadStartRequest: AppStartThreadRequest? = null,
    val createdThreadKey: ThreadKey? = null,
)

/** Native attachment ownership only. Rust still decides dispatch and retry outcomes. */
class ComposerDraftRecoveryStore internal constructor(
    persistence: ComposerDraftRecoveryPersistence? = null,
    private val persistenceFactory: () -> ComposerDraftRecoveryPersistence? = { persistence },
) {
    private val mutex = Mutex()
    private var persistence: ComposerDraftRecoveryPersistence? = null
    private var loaded = false
    private var nextId = 0L
    private val _entries = MutableStateFlow<List<RecoverableComposerDraft>>(emptyList())
    val entries = _entries.asStateFlow()
    private val _storageError = MutableStateFlow<String?>(null)
    val storageError = _storageError.asStateFlow()
    private var storageBlocked = false

    suspend fun load() = transaction { }

    private fun loadOnce() {
        if (loaded) return
        loaded = true
        try {
            persistence = persistenceFactory()
            persistence?.read()?.let { bytes ->
                try {
                    _entries.value = ComposerDraftRecoveryCodec.decode(bytes).map {
                        if (it.status == ComposerDraftRecoveryStatus.SUBMITTING) {
                            it.copy(status = ComposerDraftRecoveryStatus.UNCONFIRMED)
                        } else it
                    }
                    nextId = _entries.value.maxOfOrNull { it.id } ?: 0L
                } finally {
                    bytes.fill(0)
                }
            }
        } catch (_: Exception) {
            blockStorage()
        }
    }

    // Keep the read/modify/commit sequence together, including cancellation after rename.
    private suspend fun <T> transaction(operation: () -> T): T = withContext(Dispatchers.IO) {
        mutex.withLock {
            loadOnce()
            operation()
        }
    }

    suspend fun submit(id: Long, operation: suspend () -> Unit) {
        try {
            transaction {
                if (storageBlocked) throw ComposerDraftPersistenceException()
                check(_entries.value.any { it.id == id && it.status == ComposerDraftRecoveryStatus.SUBMITTING }) {
                    "Only an explicitly prepared composer submission can be sent."
                }
            }
        } catch (e: CancellationException) {
            withContext(NonCancellable) {
                transaction {
                    if (_entries.value.any { it.id == id && it.status == ComposerDraftRecoveryStatus.SUBMITTING }) {
                        markUnconfirmed(id)
                    }
                }
            }
            throw e
        }
        try {
            operation()
            complete(id)
        } catch (e: Exception) {
            withContext(NonCancellable) { unconfirmed(id) }
            throw e
        }
    }

    suspend fun begin(
        destination: ComposerDraftDestination,
        draft: AppModel.ComposerDraft,
        payload: AppComposerPayload,
        threadStartRequest: AppStartThreadRequest? = null,
    ): Long {
        var committedId: Long? = null
        return try {
            transaction {
                val entry = RecoverableComposerDraft(
                    id = Math.incrementExact(nextId),
                    destination = destination,
                    draft = draft,
                    status = ComposerDraftRecoveryStatus.SUBMITTING,
                    payload = payload,
                    threadStartRequest = threadStartRequest,
                )
                // A restored draft stays durable until an explicit submission replaces it.
                val retained = _entries.value.filterNot {
                    it.status == ComposerDraftRecoveryStatus.SAVED && it.destination == destination &&
                        sameContent(it.draft, draft)
                }
                persist(retained + entry)
                nextId = entry.id
                committedId = entry.id
                entry.id
            }
        } catch (e: CancellationException) {
            // The atomic commit may finish after cancellation; no caller received permission to send.
            committedId?.let { withContext(NonCancellable) { unconfirmed(it) } }
            throw e
        }
    }

    suspend fun threadCreated(id: Long, key: ThreadKey) = transaction {
        persist(_entries.value.map { if (it.id == id) it.copy(createdThreadKey = key) else it })
    }

    suspend fun complete(id: Long) = transaction {
        if (_entries.value.none { it.id == id }) return@transaction
        persist(_entries.value.filterNot { it.id == id })
    }

    suspend fun unconfirmed(id: Long) = transaction {
        markUnconfirmed(id)
    }

    private fun markUnconfirmed(id: Long) {
        val uncertain = _entries.value.map {
            if (it.id == id) it.copy(status = ComposerDraftRecoveryStatus.UNCONFIRMED) else it
        }
        try {
            persist(uncertain)
        } catch (_: ComposerDraftPersistenceException) {
            // The last durable SUBMITTING entry also restores as UNCONFIRMED.
            _entries.value = uncertain
        }
    }

    suspend fun restore(id: Long, current: AppModel.ComposerDraft): AppModel.ComposerDraft? = transaction {
        val entry = _entries.value.firstOrNull {
            it.id == id && it.status != ComposerDraftRecoveryStatus.SUBMITTING
        } ?: return@transaction null
        var retained = _entries.value.map {
            if (it.id == id) it.copy(status = ComposerDraftRecoveryStatus.SAVED) else it
        }
        var replacementId = nextId
        if (!current.isEmpty && !sameContent(current, entry.draft)) {
            replacementId = Math.incrementExact(nextId)
            retained = retained + RecoverableComposerDraft(
                id = replacementId,
                destination = entry.destination,
                draft = current,
                status = ComposerDraftRecoveryStatus.SAVED,
            )
        }
        persist(retained)
        nextId = replacementId
        entry.draft
    }

    suspend fun discard(id: Long) = transaction {
        if (_entries.value.none { it.id == id && it.status != ComposerDraftRecoveryStatus.SUBMITTING }) return@transaction
        persist(_entries.value.filterNot { it.id == id })
    }

    private fun persist(entries: List<RecoverableComposerDraft>) {
        if (storageBlocked) throw ComposerDraftPersistenceException()
        try {
            if (persistence != null) {
                val bytes = ComposerDraftRecoveryCodec.encode(entries)
                try {
                    persistence!!.replace(bytes)
                } finally {
                    bytes.fill(0)
                }
            }
            _entries.value = entries
        } catch (_: Exception) {
            blockStorage()
            throw ComposerDraftPersistenceException()
        }
    }

    private fun blockStorage() {
        storageBlocked = true
        _storageError.value = COMPOSER_RECOVERY_STORAGE_ERROR
    }
}

private fun sameContent(first: AppModel.ComposerDraft, second: AppModel.ComposerDraft): Boolean =
    first.text == second.text && first.fileAttachments == second.fileAttachments &&
        first.attachment?.mimeType == second.attachment?.mimeType &&
        (first.attachment?.data.contentEquals(second.attachment?.data))

internal const val COMPOSER_RECOVERY_STORAGE_ERROR =
    "Draft recovery could not be saved or opened. Your editor and existing recovery file were kept. " +
        "Check device storage and restart Remora before sending or restoring. Do not clear app data."

internal class ComposerDraftPersistenceException : IllegalStateException(COMPOSER_RECOVERY_STORAGE_ERROR)
