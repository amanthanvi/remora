package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Test

class LegacyV1SecretPurgeTest {
    @Test
    fun purgeAlwaysTargetsExactLegacyCredentialNamespace() {
        val deletedNames = mutableListOf<String>()
        val deleter = LegacySharedPreferencesDeleter { name ->
            deletedNames += name
            true
        }

        LegacyV1SecretPurge.purge(deleter)
        LegacyV1SecretPurge.purge(deleter)

        assertEquals(listOf("alleycat_credentials", "alleycat_credentials"), deletedNames)
    }
}
