package com.remora.android.state

import android.content.Context

internal fun interface LegacySharedPreferencesDeleter {
    fun deleteSharedPreferences(name: String): Boolean
}

/** Removes the retired v1 credential namespace before any runtime store opens. */
object LegacyV1SecretPurge {
    internal const val LEGACY_CREDENTIALS_NAME = "alleycat_credentials"

    fun purge(context: Context) {
        purge(LegacySharedPreferencesDeleter(context::deleteSharedPreferences))
    }

    internal fun purge(deleter: LegacySharedPreferencesDeleter) {
        deleter.deleteSharedPreferences(LEGACY_CREDENTIALS_NAME)
    }
}
