package com.remora.android.state

import java.nio.ByteBuffer
import uniffi.codex_mobile_client.AppRelaySecretCasOutcome
import uniffi.codex_mobile_client.AppRelaySecretCreateOutcome
import uniffi.codex_mobile_client.AppRelaySecretReadException
import uniffi.codex_mobile_client.AppRelaySecretRevision
import uniffi.codex_mobile_client.AppRelaySecretWriteOutcome

/** Revisioned opaque custody. The file backend and cipher own platform I/O, not relay policy. */
internal class RelaySecretStore(
    private val journal: RemoraLinkJournalStore,
    private val encrypt: (ByteArray) -> ByteArray,
    private val decrypt: (ByteArray) -> ByteArray,
    private val reservedAnchor: Boolean = false,
    private val verifyFence: (RemoraLinkJournalSnapshot?) -> Unit = {},
) {
    fun read(alias: String): ByteArray = try {
        withState(alias) { state ->
            state.entries[alias]?.value?.copyOf() ?: throw AppRelaySecretReadException.Missing()
        }
    } catch (missing: AppRelaySecretReadException.Missing) {
        throw missing
    } catch (_: Exception) {
        throw AppRelaySecretReadException.Unavailable()
    }

    fun revision(alias: String): AppRelaySecretRevision = runCatching {
        withState(alias) { state ->
            state.entries[alias]?.let { AppRelaySecretRevision.Found(it.revision) }
                ?: AppRelaySecretRevision.Missing
        }
    }.getOrDefault(AppRelaySecretRevision.Unavailable)

    fun write(alias: String, value: ByteArray): AppRelaySecretWriteOutcome = runCatching {
        check(!reservedAnchor)
        withState(alias) { state ->
            if (state.entries[alias]?.value?.contentEquals(value) == true) {
                return@withState AppRelaySecretWriteOutcome.APPLIED
            }
            val revision = nextRevision(state.entries[alias]?.revision)
            state.replace(alias, revision, value)
            persist(state)
            AppRelaySecretWriteOutcome.APPLIED
        }
    }.getOrDefault(AppRelaySecretWriteOutcome.UNAVAILABLE)

    fun createIfAbsent(alias: String, value: ByteArray): AppRelaySecretCreateOutcome = runCatching {
        withState(alias) { state ->
            if (state.entries.containsKey(alias)) {
                AppRelaySecretCreateOutcome.ALREADY_EXISTS
            } else {
                state.replace(alias, 1uL, value)
                persist(state)
                AppRelaySecretCreateOutcome.CREATED
            }
        }
    }.getOrDefault(AppRelaySecretCreateOutcome.UNAVAILABLE)

    fun compareAndSwap(
        alias: String,
        expectedRevision: ULong?,
        replacementRevision: ULong,
        value: ByteArray,
    ): AppRelaySecretCasOutcome = change(alias, expectedRevision, replacementRevision, value)

    fun compareAndTombstone(
        alias: String,
        expectedRevision: ULong?,
        replacementRevision: ULong,
    ): AppRelaySecretCasOutcome = if (reservedAnchor) {
        AppRelaySecretCasOutcome.UNAVAILABLE
    } else {
        change(alias, expectedRevision, replacementRevision, null)
    }

    fun delete(alias: String): AppRelaySecretWriteOutcome = runCatching {
        check(!reservedAnchor)
        withState(alias) { state ->
            state.entries.remove(alias)?.let {
                it.value?.fill(0)
                persist(state)
            }
            AppRelaySecretWriteOutcome.APPLIED
        }
    }.getOrDefault(AppRelaySecretWriteOutcome.UNAVAILABLE)

    private fun change(
        alias: String,
        expectedRevision: ULong?,
        replacementRevision: ULong,
        value: ByteArray?,
    ): AppRelaySecretCasOutcome = runCatching {
        require(replacementRevision > (expectedRevision ?: 0uL))
        withState(alias) { state ->
            if (state.entries[alias]?.revision != expectedRevision) {
                AppRelaySecretCasOutcome.CONFLICT
            } else {
                state.replace(alias, replacementRevision, value)
                persist(state)
                AppRelaySecretCasOutcome.STORED
            }
        }
    }.getOrDefault(AppRelaySecretCasOutcome.UNAVAILABLE)

    private fun <T> withState(alias: String, operation: (State) -> T): T = synchronized(storageLock) {
        require(validAlias(alias) && (alias == RELAY_ROLLBACK_ANCHOR_ALIAS) == reservedAnchor)
        val snapshot = when (val loaded = journal.load()) {
            RemoraLinkJournalLoadStatus.Missing -> null
            is RemoraLinkJournalLoadStatus.Loaded -> loaded.snapshot
            else -> error("Relay custody unavailable")
        }
        val state = if (snapshot == null) {
            State(null, linkedMapOf())
        } else {
            val ciphertext = snapshot.opaquePayload
            val plaintext = try { decrypt(ciphertext) } finally { ciphertext.fill(0) }
            try { decode(snapshot.revision, plaintext) } finally { plaintext.fill(0) }
        }
        try {
            verifyFence(snapshot)
            operation(state)
        } finally {
            state.entries.values.forEach { it.value?.fill(0) }
        }
    }

    private fun persist(state: State) {
        val plaintext = encode(state)
        val ciphertext = try { encrypt(plaintext) } finally { plaintext.fill(0) }
        try {
            val replacement = RemoraLinkJournalSnapshot(nextRevision(state.outerRevision), ciphertext)
            check(journal.compareAndSwap(state.outerRevision, replacement) == RemoraLinkJournalCasStatus.STORED)
            verifyFence(replacement)
        } finally {
            ciphertext.fill(0)
        }
    }

    private class Entry(val revision: ULong, val value: ByteArray?)

    private class State(val outerRevision: ULong?, val entries: MutableMap<String, Entry>) {
        fun replace(alias: String, revision: ULong, value: ByteArray?) {
            require(value == null || value.size in 1..MAX_SECRET_BYTES)
            entries.put(alias, Entry(revision, value?.copyOf()))?.value?.fill(0)
        }
    }

    private fun encode(state: State): ByteArray {
        require(state.entries.size <= MAX_ENTRIES)
        val size = state.entries.entries.fold(16L) { total, (alias, entry) ->
            total + 4 + alias.toByteArray(Charsets.UTF_8).size + 8 + 4 + (entry.value?.size ?: 0)
        }
        require(size <= MAX_ARCHIVE_BYTES)
        val buffer = ByteBuffer.allocate(size.toInt()).putInt(MAGIC)
            .putLong(nextRevision(state.outerRevision).toLong()).putInt(state.entries.size)
        state.entries.forEach { (alias, entry) ->
            val name = alias.toByteArray(Charsets.UTF_8)
            buffer.putInt(name.size).put(name).putLong(entry.revision.toLong())
                .putInt(entry.value?.size ?: -1)
            entry.value?.let(buffer::put)
        }
        return buffer.array()
    }

    private fun decode(outerRevision: ULong, plaintext: ByteArray): State {
        require(plaintext.size in 16..MAX_ARCHIVE_BYTES)
        val buffer = ByteBuffer.wrap(plaintext)
        require(buffer.int == MAGIC)
        require(buffer.long.toULong() == outerRevision)
        val count = buffer.int
        require(count in 0..MAX_ENTRIES)
        val entries = linkedMapOf<String, Entry>()
        try {
            repeat(count) {
                val nameLength = buffer.int
                require(nameLength in 1..MAX_ALIAS_BYTES && nameLength <= buffer.remaining())
                val name = ByteArray(nameLength).also(buffer::get)
                val alias = Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(name)).toString()
                require(validAlias(alias) && alias !in entries)
                require((alias == RELAY_ROLLBACK_ANCHOR_ALIAS) == reservedAnchor)
                val revision = buffer.long.toULong()
                require(revision != 0uL)
                val length = buffer.int
                require(length == -1 || length in 1..MAX_SECRET_BYTES)
                require(length <= buffer.remaining())
                val value = if (length == -1) null else ByteArray(length).also(buffer::get)
                entries[alias] = Entry(revision, value)
            }
            require(!buffer.hasRemaining())
            return State(outerRevision, entries)
        } catch (failure: Throwable) {
            entries.values.forEach { it.value?.fill(0) }
            throw failure
        }
    }

    private companion object {
        val storageLock = Any()
        const val MAGIC = 0x52534331
        const val MAX_ENTRIES = 4096
        const val MAX_SECRET_BYTES = 4096
        const val MAX_ALIAS_BYTES = 512
        const val MAX_ARCHIVE_BYTES = 1024 * 1024

        fun validAlias(alias: String): Boolean = alias.isNotEmpty() &&
            alias.toByteArray(Charsets.UTF_8).size <= MAX_ALIAS_BYTES && alias.none(Char::isISOControl)

        fun nextRevision(current: ULong?): ULong {
            check(current != ULong.MAX_VALUE)
            return (current ?: 0uL) + 1uL
        }
    }
}

internal const val RELAY_ROLLBACK_ANCHOR_ALIAS = "remora_relay_journal_anchor_v1"
