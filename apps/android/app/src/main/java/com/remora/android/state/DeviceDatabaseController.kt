package com.remora.android.state

import android.content.Context
import com.remora.android.BuildConfig
import java.io.File
import java.security.SecureRandom
import java.util.Base64
import uniffi.codex_mobile_client.DeviceDatabaseBridge

internal class DeviceDatabaseMasterKeyStore(
    private val secretStore: RemoraLinkSecretStore,
    private val entropy: () -> ByteArray = {
        ByteArray(KEY_BYTES).also(SecureRandom()::nextBytes)
    },
) {
    fun loadOrCreate(): ByteArray = synchronized(masterKeyLock) {
        when (val result = secretStore.load(MASTER_KEY_ALIAS)) {
            is RemoraLinkSecretReadStatus.Available -> decode(result.opaqueSecret)
            RemoraLinkSecretReadStatus.Missing -> create()
            RemoraLinkSecretReadStatus.InvalidOpaqueAlias,
            RemoraLinkSecretReadStatus.StorageFailure ->
                throw IllegalStateException("device database master key is unavailable")
        }
    }

    private fun create(): ByteArray {
        val candidate = entropy()
        require(candidate.size == KEY_BYTES) { "device database master key must be 32 bytes" }
        val encoded = Base64.getUrlEncoder().withoutPadding().encodeToString(candidate)
        return when (secretStore.replace(MASTER_KEY_ALIAS, encoded)) {
            RemoraLinkSecretMutationStatus.STORED -> candidate
            else -> {
                candidate.fill(0)
                throw IllegalStateException("device database master key could not be persisted")
            }
        }
    }

    private fun decode(encoded: String): ByteArray = try {
        Base64.getUrlDecoder().decode(encoded).also {
            require(it.size == KEY_BYTES) { "stored device database master key is corrupt" }
        }
    } catch (error: IllegalArgumentException) {
        throw IllegalStateException("stored device database master key is corrupt", error)
    }

    private companion object {
        const val KEY_BYTES = 32
        const val MASTER_KEY_ALIAS = "device-database-master-key-v1"
        val masterKeyLock = Any()
    }
}

internal data class DeviceDatabaseOpenResult(
    val database: DeviceDatabaseBridge,
    val didRebuild: Boolean,
)

/**
 * Opens the Rust-owned cache with a device-bound key. Both the wrapped key and
 * ciphertext database live under noBackupFilesDir. Cache corruption triggers
 * one exact-file rebuild; secure key material is never rotated automatically.
 */
internal class DeviceDatabaseController(
    context: Context,
) {
    private val directory = File(context.noBackupFilesDir, DIRECTORY_NAME)
    private val keyStore = DeviceDatabaseMasterKeyStore(
        RemoraLinkSecretStore(
            AndroidAtomicRemoraLinkSecretBackend(
                context = context,
                allowDebugEmulatorSoftwareAssurance = BuildConfig.DEBUG,
                directory = File(context.noBackupFilesDir, KEY_DIRECTORY_NAME),
                keyAlias = KEYSTORE_ALIAS,
            ),
        ),
    )

    fun open(): DeviceDatabaseOpenResult {
        check(directory.mkdirs() || directory.isDirectory) {
            "device database directory is unavailable"
        }
        val databaseFile = File(directory, DATABASE_FILE_NAME)
        val files = databaseFiles(databaseFile)
        val hadExistingCache = files.any(File::exists)
        val key = keyStore.loadOrCreate()
        return try {
            DeviceDatabaseOpenResult(
                database = openBridge(databaseFile, key),
                didRebuild = false,
            )
        } catch (error: Exception) {
            if (!hadExistingCache) throw error
            check(files.all { !it.exists() || it.delete() }) {
                "corrupt device database could not be removed"
            }
            DeviceDatabaseOpenResult(
                database = openBridge(databaseFile, key),
                didRebuild = true,
            )
        } finally {
            // Generated UniFFI lowering also wipes the array. This covers
            // failures before lowering begins.
            key.fill(0)
        }
    }

    private fun openBridge(databaseFile: File, key: ByteArray): DeviceDatabaseBridge {
        val crossingKey = key.copyOf()
        return try {
            DeviceDatabaseBridge.open(databaseFile.absolutePath, crossingKey)
        } finally {
            crossingKey.fill(0)
        }
    }

    private fun databaseFiles(databaseFile: File): List<File> = listOf(
        databaseFile,
        File(databaseFile.path + "-wal"),
        File(databaseFile.path + "-shm"),
    )

    private companion object {
        const val DIRECTORY_NAME = "device_database_v1"
        const val KEY_DIRECTORY_NAME = "device_database_key_v1"
        const val DATABASE_FILE_NAME = "workspace.sqlite3"
        const val KEYSTORE_ALIAS = "com.remora.android.device_database.v1.master_key.aes"
    }
}
