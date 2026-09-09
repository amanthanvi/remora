package com.remora.android.state

import android.content.Context
import java.io.File
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppRelayJournalBackend
import uniffi.codex_mobile_client.AppRelayJournalLoad
import uniffi.codex_mobile_client.AppRelayJournalSnapshot
import uniffi.codex_mobile_client.AppRelayJournalWriteOutcome
import uniffi.codex_mobile_client.AppRelaySecretBackend
import uniffi.codex_mobile_client.AppRelaySecretCasOutcome
import uniffi.codex_mobile_client.AppRelaySecretCreateOutcome
import uniffi.codex_mobile_client.AppRelaySecretRevision
import uniffi.codex_mobile_client.AppRelaySecretWriteOutcome

internal class AndroidRelayJournalBackend(
    private val store: RemoraLinkJournalStore,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) : AppRelayJournalBackend {
    constructor(context: Context) : this(relayJournalStore(context))

    override suspend fun load(): AppRelayJournalLoad = withContext(ioDispatcher) {
        currentCoroutineContext().ensureActive()
        when (val loaded = store.load()) {
            RemoraLinkJournalLoadStatus.Missing -> AppRelayJournalLoad.Missing
            is RemoraLinkJournalLoadStatus.Loaded -> if (loaded.snapshot.payloadSize <= MAX_RELAY_JOURNAL_BYTES) {
                AppRelayJournalLoad.Loaded(AppRelayJournalSnapshot(loaded.snapshot.revision, loaded.snapshot.opaquePayload))
            } else {
                AppRelayJournalLoad.Unavailable
            }
            else -> AppRelayJournalLoad.Unavailable
        }
    }

    override suspend fun compareAndSwap(
        expectedRevision: ULong?,
        replacement: AppRelayJournalSnapshot,
    ): AppRelayJournalWriteOutcome = withContext(ioDispatcher) {
        currentCoroutineContext().ensureActive()
        if (replacement.payload.size > MAX_RELAY_JOURNAL_BYTES) return@withContext AppRelayJournalWriteOutcome.UNAVAILABLE
        when (store.compareAndSwap(expectedRevision, RemoraLinkJournalSnapshot(replacement.revision, replacement.payload))) {
            RemoraLinkJournalCasStatus.STORED -> AppRelayJournalWriteOutcome.STORED
            RemoraLinkJournalCasStatus.CONFLICT -> AppRelayJournalWriteOutcome.CONFLICT
            else -> AppRelayJournalWriteOutcome.UNAVAILABLE
        }
    }

    private companion object {
        const val MAX_RELAY_JOURNAL_BYTES = 512 * 1024

        fun relayJournalStore(context: Context): RemoraLinkJournalStore {
            requireRelaySecurityCutover(context)
            return RemoraLinkJournalStore(AtomicFileRemoraLinkJournalBackend(
                context, File(context.noBackupFilesDir, "remora_background_relay_journal_v1"),
            ))
        }
    }
}

internal class AndroidRelaySecretBackend(
    private val secrets: RelaySecretStore,
    private val anchor: RelaySecretStore,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) : AppRelaySecretBackend {
    constructor(context: Context) : this(
        androidRelaySecretStore(context, reservedAnchor = false),
        androidRelaySecretStore(context, reservedAnchor = true),
    )

    override suspend fun read(alias: String): ByteArray {
        val pending = AtomicReference<ByteArray?>(null)
        try {
            val result = withContext(ioDispatcher) {
                currentCoroutineContext().ensureActive()
                storage(alias).read(alias).also {
                    pending.set(it)
                    currentCoroutineContext().ensureActive()
                }
            }
            check(pending.compareAndSet(result, null))
            return result
        } catch (failure: Throwable) {
            pending.getAndSet(null)?.fill(0)
            throw failure
        }
    }

    override suspend fun write(alias: String, value: ByteArray): AppRelaySecretWriteOutcome =
        withValue(value) { storage(alias).write(alias, it) }

    override suspend fun createIfAbsent(alias: String, value: ByteArray): AppRelaySecretCreateOutcome =
        withValue(value) { storage(alias).createIfAbsent(alias, it) }

    override suspend fun revision(alias: String): AppRelaySecretRevision = onIo {
        storage(alias).revision(alias)
    }

    override suspend fun compareAndSwap(
        alias: String,
        expectedRevision: ULong?,
        replacementRevision: ULong,
        value: ByteArray,
    ): AppRelaySecretCasOutcome = withValue(value) {
        storage(alias).compareAndSwap(alias, expectedRevision, replacementRevision, it)
    }

    override suspend fun compareAndTombstone(
        alias: String,
        expectedRevision: ULong?,
        replacementRevision: ULong,
    ): AppRelaySecretCasOutcome = onIo {
        storage(alias).compareAndTombstone(alias, expectedRevision, replacementRevision)
    }

    override suspend fun delete(alias: String): AppRelaySecretWriteOutcome = onIo {
        storage(alias).delete(alias)
    }

    private fun storage(alias: String): RelaySecretStore =
        if (alias == RELAY_ROLLBACK_ANCHOR_ALIAS) anchor else secrets

    private suspend fun <T> withValue(value: ByteArray, operation: (ByteArray) -> T): T {
        val owned = value.copyOf()
        return try { onIo { operation(owned) } } finally { owned.fill(0) }
    }

    private suspend fun <T> onIo(operation: () -> T): T = withContext(ioDispatcher) {
        currentCoroutineContext().ensureActive()
        operation().also { currentCoroutineContext().ensureActive() }
    }
}
