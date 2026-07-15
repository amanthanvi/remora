package com.remora.android.state

import android.content.Context
import android.net.Uri
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import com.google.gson.stream.JsonReader
import com.google.gson.stream.JsonToken
import com.remora.android.util.LLog
import kotlinx.coroutines.delay
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.io.StringReader
import java.net.HttpURLConnection
import java.net.UnknownHostException
import java.net.URL
import java.security.MessageDigest
import java.security.SecureRandom

data class ChatGPTOAuthTokenBundle(
    val accessToken: String,
    val idToken: String,
    val refreshToken: String?,
    val accountId: String,
    val planType: String?,
)

class ChatGPTOAuthException(message: String) : Exception(message)

object ChatGPTOAuth {
    const val MODE_LOGIN = "login"
    const val MODE_REMOTE_CONTROL_ENROLL = "remote_control_enroll"
    const val authIssuer = "https://auth.openai.com"
    private const val clientId = "app_EMoamEEZ73f0CkXaXp7hrann"
    private const val callbackScheme = "http"
    private const val callbackHost = "localhost"
    private const val callbackBindHost = "127.0.0.1"
    private const val callbackPort = 1455
    private const val callbackPath = "/auth/callback"
    private const val tokenExchangeMaxAttempts = 5
    private val oauthDiagnosticAllowedKeys = setOf(
        "access_token", "code", "error", "error_description", "error_uri", "expires_in",
        "id_token", "message", "refresh_token", "request_id", "scope", "status", "token_type", "type",
    )

    private enum class OAuthJsonShape {
        OBJECT,
        ARRAY,
        SCALAR,
    }

    private data class OAuthJsonStructure(
        val shape: OAuthJsonShape,
        val keys: List<String> = emptyList(),
        val count: Int = 0,
    )

    data class AuthAttempt(
        val state: String,
        val codeVerifier: String,
        val redirectUri: String,
        val authorizeUrl: String,
        val mode: String = MODE_LOGIN,
    )

    fun createLoginAttempt(): AuthAttempt {
        val state = java.util.UUID.randomUUID().toString()
        val codeVerifier = generatePkceCodeVerifier()
        val codeChallenge = generatePkceCodeChallenge(codeVerifier)
        val redirectUri = "$callbackScheme://$callbackHost:$callbackPort$callbackPath"
        val authorizeUrl = Uri.parse("$authIssuer/oauth/authorize")
            .buildUpon()
            .appendQueryParameter("response_type", "code")
            .appendQueryParameter("client_id", clientId)
            .appendQueryParameter("redirect_uri", redirectUri)
            .appendQueryParameter("scope", "openid profile email offline_access")
            .appendQueryParameter("code_challenge", codeChallenge)
            .appendQueryParameter("code_challenge_method", "S256")
            .appendQueryParameter("state", state)
            .appendQueryParameter("id_token_add_organizations", "true")
            .appendQueryParameter("codex_cli_simplified_flow", "true")
            .build()
            .toString()
        return AuthAttempt(
            state = state,
            codeVerifier = codeVerifier,
            redirectUri = redirectUri,
            authorizeUrl = authorizeUrl,
            mode = MODE_LOGIN,
        )
    }

    fun createRemoteControlEnrollmentAttempt(): AuthAttempt {
        val state = java.util.UUID.randomUUID().toString()
        val codeVerifier = generatePkceCodeVerifier()
        val codeChallenge = generatePkceCodeChallenge(codeVerifier)
        val redirectUri = "$callbackScheme://$callbackHost:$callbackPort$callbackPath"
        val authorizeUrl = Uri.parse("$authIssuer/oauth/authorize")
            .buildUpon()
            .appendQueryParameter("response_type", "code")
            .appendQueryParameter("client_id", clientId)
            .appendQueryParameter("redirect_uri", redirectUri)
            .appendQueryParameter("scope", "codex.remote_control.enroll")
            .appendQueryParameter("code_challenge", codeChallenge)
            .appendQueryParameter("code_challenge_method", "S256")
            .appendQueryParameter("state", state)
            .appendQueryParameter("originator", "Codex Desktop")
            .appendQueryParameter("reauth", "remote_control")
            .appendQueryParameter("max_age", "0")
            .appendQueryParameter("codex_cli_simplified_flow", "true")
            .build()
            .toString()
        LLog.i(
            "Slingshot",
            "remote-control step-up auth attempt created",
            fields = mapOf("state" to state, "redirectUri" to redirectUri),
        )
        return AuthAttempt(
            state = state,
            codeVerifier = codeVerifier,
            redirectUri = redirectUri,
            authorizeUrl = authorizeUrl,
            mode = MODE_REMOTE_CONTROL_ENROLL,
        )
    }

    fun isRemoteControlAuthorizationRequired(error: Throwable): Boolean {
        val message = "${error.message.orEmpty()} ${error}".lowercase()
        return message.contains("missing slingshot remote-control authorization token") ||
            message.contains("missing slingshot client session token") ||
            message.contains("remote-control authorization")
    }

    suspend fun loadStoredOrRefreshedTokens(context: Context): ChatGPTOAuthTokenBundle? {
        val appContext = context.applicationContext
        val stored = ChatGPTOAuthTokenStore(appContext).load() ?: return null
        return runCatching {
            refreshStoredTokens(appContext, stored.accountId)
        }.getOrElse {
            stored
        }
    }

    suspend fun requireStoredOrRefreshedTokens(
        context: Context,
        missingMessage: String,
    ): ChatGPTOAuthTokenBundle =
        loadStoredOrRefreshedTokens(context) ?: throw IllegalStateException(missingMessage)

    fun isCallbackUri(uri: Uri): Boolean {
        val host = uri.host?.lowercase()
        return uri.scheme == callbackScheme &&
            (host == callbackHost || host == callbackBindHost) &&
            uri.path == callbackPath
    }

    suspend fun completeAuthorization(
        context: Context,
        callbackUri: Uri,
        attempt: AuthAttempt,
    ): ChatGPTOAuthTokenBundle {
        validateCallbackUri(callbackUri)
        val error = callbackUri.getQueryParameter("error")?.trim()
        if (!error.isNullOrEmpty()) {
            val description = callbackUri.getQueryParameter("error_description")?.trim()
            throw ChatGPTOAuthException(
                description?.takeIf { it.isNotEmpty() } ?: error,
            )
        }

        val state = callbackUri.getQueryParameter("state")
        if (state != attempt.state) {
            throw ChatGPTOAuthException("ChatGPT login state did not match the original request.")
        }

        val code = callbackUri.getQueryParameter("code")?.trim()
        if (code.isNullOrEmpty()) {
            throw ChatGPTOAuthException("ChatGPT login did not return an authorization code.")
        }

        val body = listOf(
            "grant_type=authorization_code",
            "code=${Uri.encode(code)}",
            "redirect_uri=${Uri.encode(attempt.redirectUri)}",
            "client_id=${Uri.encode(clientId)}",
            "code_verifier=${Uri.encode(attempt.codeVerifier)}",
        ).joinToString("&")

        val tokens = exchangeToken(body)
        ChatGPTOAuthTokenStore(context).save(tokens)
        return tokens
    }

    suspend fun completeRemoteControlEnrollmentAuthorization(
        callbackUri: Uri,
        attempt: AuthAttempt,
    ): String {
        validateAuthorizationCallback(callbackUri, attempt)
        LLog.i(
            "Slingshot",
            "remote-control step-up auth callback received",
            fields = mapOf("state" to attempt.state),
        )
        val code = callbackUri.getQueryParameter("code")?.trim()
            ?: throw ChatGPTOAuthException("ChatGPT login did not return an authorization code.")
        if (code.isEmpty()) {
            throw ChatGPTOAuthException("ChatGPT login did not return an authorization code.")
        }

        val body = listOf(
            "grant_type=authorization_code",
            "code=${Uri.encode(code)}",
            "redirect_uri=${Uri.encode(attempt.redirectUri)}",
            "client_id=${Uri.encode(clientId)}",
            "code_verifier=${Uri.encode(attempt.codeVerifier)}",
        ).joinToString("&")

        val token = exchangeAccessToken(body)
        LLog.i(
            "Slingshot",
            "remote-control step-up token received",
            fields = mapOf("tokenLength" to token.length),
        )
        return token
    }

    suspend fun refreshStoredTokens(
        context: Context,
        previousAccountId: String?,
    ): ChatGPTOAuthTokenBundle {
        val stored = withContext(Dispatchers.IO) {
            ChatGPTOAuthTokenStore(context).load()
        } ?: throw ChatGPTOAuthException("No stored ChatGPT login is available to refresh.")
        val refreshToken = stored.refreshToken?.takeIf { it.isNotBlank() }
            ?: throw ChatGPTOAuthException("No ChatGPT refresh token is available.")
        val body = listOf(
            "grant_type=refresh_token",
            "refresh_token=${Uri.encode(refreshToken)}",
            "client_id=${Uri.encode(clientId)}",
        ).joinToString("&")
        val refreshed = exchangeToken(body)
        if (!previousAccountId.isNullOrBlank() &&
            refreshed.accountId != previousAccountId &&
            stored.accountId != previousAccountId
        ) {
            throw ChatGPTOAuthException("ChatGPT refresh returned a different account than expected.")
        }
        withContext(Dispatchers.IO) {
            ChatGPTOAuthTokenStore(context).save(refreshed)
        }
        return refreshed
    }

    private suspend fun exchangeToken(body: String): ChatGPTOAuthTokenBundle = withContext(Dispatchers.IO) {
        tokenBundleFromPayload(exchangeTokenPayloadWithRetries(body))
    }

    private suspend fun exchangeAccessToken(body: String): String = withContext(Dispatchers.IO) {
        val payload = exchangeTokenPayloadWithRetries(body)
        val accessToken = payload.optString("access_token").trim()
        if (accessToken.isEmpty()) {
            throw ChatGPTOAuthException("ChatGPT token exchange failed: missing access_token.")
        }
        accessToken
    }

    private fun tokenBundleFromPayload(payload: JSONObject): ChatGPTOAuthTokenBundle {
        val accessToken = payload.optString("access_token").trim()
        val idToken = payload.optString("id_token").trim()
        val refreshToken = payload.optString("refresh_token").trim().ifEmpty { null }
        if (accessToken.isEmpty() || idToken.isEmpty()) {
            throw ChatGPTOAuthException("ChatGPT token exchange failed: missing access_token or id_token.")
        }

        val idClaims = decodeJwtClaims(idToken)
        val accessClaims = decodeJwtClaims(accessToken)
        val accountId = resolveAccountId(idClaims, accessClaims)
        if (accountId.isEmpty()) {
            throw ChatGPTOAuthException("ChatGPT login did not include an account identifier.")
        }

        return ChatGPTOAuthTokenBundle(
            accessToken = accessToken,
            idToken = idToken,
            refreshToken = refreshToken,
            accountId = accountId,
            planType = resolvePlanType(idClaims, accessClaims),
        )
    }

    private suspend fun exchangeTokenPayloadWithRetries(body: String): JSONObject {
        var networkFailure: IOException? = null
        repeat(tokenExchangeMaxAttempts) { attemptIndex ->
            try {
                return exchangeTokenPayload(body)
            } catch (error: UnknownHostException) {
                networkFailure = error
                if (attemptIndex == tokenExchangeMaxAttempts - 1) {
                    throw ChatGPTOAuthException(
                        "ChatGPT token exchange could not reach auth.openai.com. Check the device connection and try again.",
                    )
                }
                logTokenExchangeRetry(error, attemptIndex)
                delay(tokenExchangeRetryDelayMs(attemptIndex))
            } catch (error: IOException) {
                networkFailure = error
                if (attemptIndex == tokenExchangeMaxAttempts - 1) {
                    throw ChatGPTOAuthException(
                        "ChatGPT token exchange failed: ${error.localizedMessage ?: error.message ?: error.javaClass.simpleName}",
                    )
                }
                logTokenExchangeRetry(error, attemptIndex)
                delay(tokenExchangeRetryDelayMs(attemptIndex))
            }
        }
        throw networkFailure ?: ChatGPTOAuthException("ChatGPT token exchange failed before it could start.")
    }

    private fun tokenExchangeRetryDelayMs(attemptIndex: Int): Long =
        when (attemptIndex) {
            0 -> 500L
            1 -> 1_000L
            2 -> 2_000L
            else -> 4_000L
        }

    private fun logTokenExchangeRetry(error: IOException, attemptIndex: Int) {
        LLog.w(
            "ChatGPTOAuth",
            "ChatGPT token exchange network failure; retrying",
            fields = mapOf(
                "attempt" to (attemptIndex + 1),
                "maxAttempts" to tokenExchangeMaxAttempts,
                "errorType" to error.javaClass.simpleName,
                "message" to (error.localizedMessage ?: error.message).orEmpty().take(160),
                "nextDelayMs" to tokenExchangeRetryDelayMs(attemptIndex),
            ),
        )
    }

    private fun exchangeTokenPayload(body: String): JSONObject {
        val url = URL("$authIssuer/oauth/token")
        val connection = (url.openConnection() as HttpURLConnection).apply {
            requestMethod = "POST"
            connectTimeout = 20_000
            readTimeout = 20_000
            doOutput = true
            setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
        }

        try {
            LLog.i(
                "ChatGPTOAuth",
                "ChatGPT token exchange request",
                fields = mapOf(
                    "url" to url.toString(),
                    "grantType" to formValue(body, "grant_type"),
                ),
            )
            connection.outputStream.use { output ->
                output.write(body.toByteArray(Charsets.UTF_8))
            }

            val status = connection.responseCode
            val stream = if (status in 200..299) connection.inputStream else connection.errorStream
            val responseText = stream?.use { input ->
                BufferedReader(InputStreamReader(input)).readText()
            }.orEmpty()

            LLog.i(
                "ChatGPTOAuth",
                "ChatGPT token exchange response",
                fields = mapOf(
                    "status" to status,
                    "keys" to jsonObjectKeys(responseText).joinToString(","),
                ),
            )
            if (status !in 200..299) {
                val diagnosticBody = oauthErrorResponseMetadata(responseText)
                LLog.w(
                    "ChatGPTOAuth",
                    "ChatGPT token exchange failed",
                    fields = mapOf("status" to status, "body" to diagnosticBody),
                )
                throw ChatGPTOAuthException(
                    "ChatGPT token exchange failed ($status): $diagnosticBody",
                )
            }

            return JSONObject(responseText)
        } finally {
            connection.disconnect()
        }
    }

    private fun formValue(body: String, name: String): String? =
        body.split("&")
            .firstOrNull { it.substringBefore("=") == name }
            ?.substringAfter("=", "")
            ?.let(Uri::decode)

    internal fun jsonObjectKeys(text: String): List<String> = try {
        val structure = strictJsonStructure(text)
        if (structure.shape == OAuthJsonShape.OBJECT) structure.keys else emptyList()
    } catch (_: Exception) {
        emptyList()
    }

    internal fun oauthErrorResponseMetadata(text: String): String = try {
        val structure = strictJsonStructure(text)
        val metadata = when (structure.shape) {
            OAuthJsonShape.OBJECT -> {
                "<JSON object response omitted; keys=" +
                    structure.keys.ifEmpty { listOf("none") }.joinToString(",") +
                    ">"
            }
            OAuthJsonShape.ARRAY -> "<JSON array response omitted; count=" + structure.count + ">"
            OAuthJsonShape.SCALAR -> "<JSON scalar response omitted>"
        }
        metadata.take(300)
    } catch (_: Exception) {
        "<non-JSON response omitted>"
    }

    private fun strictJsonStructure(text: String): OAuthJsonStructure {
        validateStrictJsonLexemes(text)
        return JsonReader(StringReader(text)).use { reader ->
            reader.isLenient = false
            val structure = when (reader.peek()) {
                JsonToken.BEGIN_OBJECT -> {
                    val keys = linkedSetOf<String>()
                    reader.beginObject()
                    while (reader.hasNext()) {
                        keys += safeOAuthDiagnosticKey(reader.nextName())
                        reader.skipValue()
                    }
                    reader.endObject()
                    OAuthJsonStructure(
                        shape = OAuthJsonShape.OBJECT,
                        keys = boundedOAuthDiagnosticKeys(keys),
                    )
                }
                JsonToken.BEGIN_ARRAY -> {
                    var count = 0
                    reader.beginArray()
                    while (reader.hasNext()) {
                        reader.skipValue()
                        if (count < Int.MAX_VALUE) count += 1
                    }
                    reader.endArray()
                    OAuthJsonStructure(shape = OAuthJsonShape.ARRAY, count = count)
                }
                JsonToken.STRING, JsonToken.NUMBER -> {
                    reader.nextString()
                    OAuthJsonStructure(shape = OAuthJsonShape.SCALAR)
                }
                JsonToken.BOOLEAN -> {
                    reader.nextBoolean()
                    OAuthJsonStructure(shape = OAuthJsonShape.SCALAR)
                }
                JsonToken.NULL -> {
                    reader.nextNull()
                    OAuthJsonStructure(shape = OAuthJsonShape.SCALAR)
                }
                else -> throw IllegalArgumentException("response is not JSON")
            }
            require(reader.peek() == JsonToken.END_DOCUMENT) { "trailing response content" }
            structure
        }
    }

    private fun validateStrictJsonLexemes(text: String) {
        var inString = false
        var escaped = false
        var index = 0
        while (index < text.length) {
            val character = text[index]
            if (inString) {
                when {
                    escaped && character == 'u' -> {
                        require(index + 4 < text.length) { "incomplete JSON unicode escape" }
                        require(
                            text.substring(index + 1, index + 5).all { it.isHexDigit() },
                        ) { "invalid JSON unicode escape" }
                        escaped = false
                        index += 5
                        continue
                    }
                    escaped -> {
                        require(character in "\"\\/bfnrt") { "invalid JSON escape" }
                        escaped = false
                    }
                    character == '\\' -> escaped = true
                    character == '"' -> inString = false
                    character.code <= 0x1f -> throw IllegalArgumentException(
                        "unescaped control character in JSON string",
                    )
                }
                index += 1
                continue
            }

            when {
                character == '"' -> {
                    inString = true
                    index += 1
                }
                character in 'A'..'Z' || character in 'a'..'z' -> {
                    val tokenStart = index
                    while (
                        index < text.length &&
                        (text[index] in 'A'..'Z' || text[index] in 'a'..'z')
                    ) {
                        index += 1
                    }
                    val token = text.substring(tokenStart, index)
                    if (
                        token != token.lowercase() &&
                        token.lowercase() in setOf("true", "false", "null")
                    ) {
                        throw IllegalArgumentException("case-variant JSON literal")
                    }
                }
                character.code <= 0x1f &&
                    character != '\t' &&
                    character != '\n' &&
                    character != '\r' -> throw IllegalArgumentException(
                    "invalid JSON control character",
                )
                else -> index += 1
            }
        }
        require(!inString && !escaped) { "unterminated JSON string" }
    }

    private fun Char.isHexDigit(): Boolean =
        this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

    private fun safeOAuthDiagnosticKey(key: String): String {
        val normalized = key.lowercase()
        return normalized.takeIf(oauthDiagnosticAllowedKeys::contains) ?: "<other>"
    }

    private fun boundedOAuthDiagnosticKeys(keys: Set<String>): List<String> {
        val normalized = keys.sorted()
        return if (normalized.size <= 12) normalized else normalized.take(12) + "<truncated>"
    }

    private fun validateAuthorizationCallback(callbackUri: Uri, attempt: AuthAttempt) {
        validateCallbackUri(callbackUri)
        val error = callbackUri.getQueryParameter("error")?.trim()
        if (!error.isNullOrEmpty()) {
            val description = callbackUri.getQueryParameter("error_description")?.trim()
            throw ChatGPTOAuthException(
                description?.takeIf { it.isNotEmpty() } ?: error,
            )
        }

        val state = callbackUri.getQueryParameter("state")
        if (state != attempt.state) {
            throw ChatGPTOAuthException("ChatGPT login state did not match the original request.")
        }
    }

    private fun validateCallbackUri(callbackUri: Uri) {
        if (!isCallbackUri(callbackUri)) {
            throw ChatGPTOAuthException("ChatGPT login returned an invalid callback.")
        }
    }

    private fun resolveAccountId(idClaims: JSONObject, accessClaims: JSONObject): String {
        val candidates = listOf(
            idClaims.optString("chatgpt_account_id"),
            accessClaims.optString("chatgpt_account_id"),
            idClaims.optString("organization_id"),
            accessClaims.optString("organization_id"),
        )
        return candidates.firstOrNull { it.isNotBlank() }?.trim().orEmpty()
    }

    private fun resolvePlanType(idClaims: JSONObject, accessClaims: JSONObject): String? {
        val candidates = listOf(
            accessClaims.optString("chatgpt_plan_type"),
            idClaims.optString("chatgpt_plan_type"),
        )
        return candidates.firstOrNull { it.isNotBlank() }?.trim()
    }

    private fun decodeJwtClaims(jwt: String): JSONObject {
        val parts = jwt.split(".")
        if (parts.size < 2) return JSONObject()
        return try {
            val decoded = Base64.decode(parts[1], Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
            val obj = JSONObject(String(decoded, Charsets.UTF_8))
            obj.optJSONObject("https://api.openai.com/auth") ?: obj
        } catch (_: Exception) {
            JSONObject()
        }
    }

    private fun generatePkceCodeVerifier(): String {
        val bytes = ByteArray(32)
        SecureRandom().nextBytes(bytes)
        return Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
    }

    private fun generatePkceCodeChallenge(codeVerifier: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
            .digest(codeVerifier.toByteArray(Charsets.UTF_8))
        return Base64.encodeToString(digest, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
    }
}

class ChatGPTOAuthTokenStore(context: Context) {
    private val prefs = openEncryptedPrefsOrReset(context, PREFS_NAME)

    fun load(): ChatGPTOAuthTokenBundle? {
        val raw = prefs.getString(KEY_TOKENS, null) ?: return null
        return try {
            val obj = JSONObject(raw)
            ChatGPTOAuthTokenBundle(
                accessToken = obj.getString("accessToken"),
                idToken = obj.getString("idToken"),
                refreshToken = obj.optString("refreshToken").takeIf { it.isNotBlank() },
                accountId = obj.getString("accountId"),
                planType = obj.optString("planType").takeIf { it.isNotBlank() },
            )
        } catch (_: Exception) {
            null
        }
    }

    fun save(tokens: ChatGPTOAuthTokenBundle) {
        val payload = JSONObject().apply {
            put("accessToken", tokens.accessToken)
            put("idToken", tokens.idToken)
            put("accountId", tokens.accountId)
            tokens.refreshToken?.let { put("refreshToken", it) }
            tokens.planType?.let { put("planType", it) }
        }
        prefs.edit().putString(KEY_TOKENS, payload.toString()).apply()
    }

    fun clear() {
        prefs.edit().remove(KEY_TOKENS).apply()
    }

    companion object {
        private const val PREFS_NAME = "remora_chatgpt_auth"
        private const val KEY_TOKENS = "tokens"
    }
}
