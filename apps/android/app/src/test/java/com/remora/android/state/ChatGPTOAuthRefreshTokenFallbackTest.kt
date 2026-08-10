package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Test

class ChatGPTOAuthRefreshTokenFallbackTest {
    @Test
    fun missingRefreshTokenUsesStoredFallbackWithoutChangingOtherFields() {
        val refreshed = ChatGPTOAuthTokenBundle(
            accessToken = "access_new",
            idToken = "id_new",
            refreshToken = null,
            accountId = "acct_expected",
            planType = "plan_expected",
        )

        val resolved = ChatGPTOAuth.withRefreshTokenFallback(
            refreshed = refreshed,
            fallbackRefreshToken = "refresh_stored",
        )

        assertEquals(
            ChatGPTOAuthTokenBundle(
                accessToken = "access_new",
                idToken = "id_new",
                refreshToken = "refresh_stored",
                accountId = "acct_expected",
                planType = "plan_expected",
            ),
            resolved,
        )
    }

    @Test
    fun rotatedRefreshTokenRemainsAuthoritativeWithoutChangingOtherFields() {
        val refreshed = ChatGPTOAuthTokenBundle(
            accessToken = "access_new",
            idToken = "id_new",
            refreshToken = "refresh_rotated",
            accountId = "acct_expected",
            planType = "plan_expected",
        )

        val resolved = ChatGPTOAuth.withRefreshTokenFallback(
            refreshed = refreshed,
            fallbackRefreshToken = "refresh_stored",
        )

        assertEquals(refreshed, resolved)
    }
}
