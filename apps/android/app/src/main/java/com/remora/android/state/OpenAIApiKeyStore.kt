package com.remora.android.state

import android.annotation.SuppressLint
import android.content.Context
import android.system.Os

class OpenAIApiKeyStore(context: Context) {
    private val prefs = openEncryptedPrefsOrReset(context, PREFS_NAME)

    fun hasStoredKey(): Boolean = !load().isNullOrBlank()
    fun hasStoredBaseUrl(): Boolean = !loadBaseUrl().isNullOrBlank()

    fun load(): String? {
        val raw = prefs.getString(KEY_API_KEY, null)?.trim()
        return raw?.takeIf { it.isNotEmpty() }
    }

    fun loadBaseUrl(): String? {
        val raw = prefs.getString(KEY_BASE_URL, null)?.trim()
        return raw?.takeIf { it.isNotEmpty() }
    }

    @SuppressLint("ApplySharedPref", "UseKtx") // Credential/environment state must change only after durable storage.
    fun save(apiKey: String) {
        val trimmed = apiKey.trim()
        check(prefs.edit().putString(KEY_API_KEY, trimmed).commit()) {
            "Could not persist the OpenAI API key"
        }
        applyToEnvironment()
    }

    @SuppressLint("ApplySharedPref", "UseKtx") // See save(): the environment must not outrun durable state.
    fun saveBaseUrl(baseUrl: String) {
        val trimmed = baseUrl.trim()
        check(prefs.edit().putString(KEY_BASE_URL, trimmed).commit()) {
            "Could not persist the OpenAI base URL"
        }
        applyToEnvironment()
    }

    @SuppressLint("ApplySharedPref", "UseKtx") // Do not revoke the process key unless durable revocation succeeds.
    fun clear() {
        check(prefs.edit().remove(KEY_API_KEY).commit()) {
            "Could not remove the OpenAI API key"
        }
        try {
            Os.unsetenv(API_KEY_ENV_KEY)
        } catch (_: Exception) {
        }
    }

    @SuppressLint("ApplySharedPref", "UseKtx") // Do not revoke the process URL unless durable revocation succeeds.
    fun clearBaseUrl() {
        check(prefs.edit().remove(KEY_BASE_URL).commit()) {
            "Could not remove the OpenAI base URL"
        }
        try {
            Os.unsetenv(BASE_URL_ENV_KEY)
        } catch (_: Exception) {
        }
    }

    fun applyToEnvironment() {
        val key = load()
        val baseUrl = loadBaseUrl()
        try {
            if (key.isNullOrEmpty()) {
                Os.unsetenv(API_KEY_ENV_KEY)
            } else {
                Os.setenv(API_KEY_ENV_KEY, key, true)
            }

            if (baseUrl.isNullOrEmpty()) {
                Os.unsetenv(BASE_URL_ENV_KEY)
            } else {
                Os.setenv(BASE_URL_ENV_KEY, baseUrl, true)
            }
        } catch (_: Exception) {
        }
    }

    companion object {
        private const val PREFS_NAME = "remora_openai_api_key"
        private const val KEY_API_KEY = "openai_api_key"
        private const val KEY_BASE_URL = "openai_base_url"
        private const val API_KEY_ENV_KEY = "OPENAI_API_KEY"
        private const val BASE_URL_ENV_KEY = "OPENAI_BASE_URL"
    }
}
