package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.fail
import org.junit.Test

class ChatGPTOAuthAccountBindingTest {
    private val refreshed = ChatGPTOAuthTokenBundle(
        accessToken = "access_new",
        idToken = "id_new",
        refreshToken = "refresh_new",
        accountId = "acct_expected",
        planType = "plan_expected",
    )

    @Test
    fun matchingAccountSavesOnceAndReturnsBundle() {
        var saveCount = 0
        var saved: ChatGPTOAuthTokenBundle? = null

        val result = ChatGPTOAuth.persistValidatedRefresh(
            refreshed = refreshed,
            expectedAccountId = "acct_expected",
            storedAccountId = "acct_expected",
            save = {
                saved = it
                saveCount += 1
            },
        )

        assertSame(refreshed, result)
        assertSame(refreshed, saved)
        assertEquals(1, saveCount)
    }

    @Test
    fun refreshedAccountMismatchThrowsWithoutSaving() {
        var saveCount = 0

        try {
            ChatGPTOAuth.persistValidatedRefresh(
                refreshed = refreshed.copy(accountId = "acct_other"),
                expectedAccountId = "acct_expected",
                storedAccountId = "acct_expected",
                save = { saveCount += 1 },
            )
            fail("Expected ChatGPTOAuthException")
        } catch (_: ChatGPTOAuthException) {
        }

        assertEquals(0, saveCount)
    }

    @Test
    fun storedAccountMismatchThrowsWithoutSaving() {
        var saveCount = 0

        try {
            ChatGPTOAuth.persistValidatedRefresh(
                refreshed = refreshed,
                expectedAccountId = "acct_expected",
                storedAccountId = "acct_other",
                save = { saveCount += 1 },
            )
            fail("Expected ChatGPTOAuthException")
        } catch (_: ChatGPTOAuthException) {
        }

        assertEquals(0, saveCount)
    }

    @Test
    fun nullExpectedAccountPreservesUnboundBehavior() {
        var saveCount = 0

        val result = ChatGPTOAuth.persistValidatedRefresh(
            refreshed = refreshed,
            expectedAccountId = null,
            storedAccountId = "acct_other",
            save = { saveCount += 1 },
        )

        assertSame(refreshed, result)
        assertEquals(1, saveCount)
    }

    @Test
    fun blankExpectedAccountPreservesUnboundBehavior() {
        var saveCount = 0

        val result = ChatGPTOAuth.persistValidatedRefresh(
            refreshed = refreshed,
            expectedAccountId = "   ",
            storedAccountId = "acct_other",
            save = { saveCount += 1 },
        )

        assertSame(refreshed, result)
        assertEquals(1, saveCount)
    }
}
