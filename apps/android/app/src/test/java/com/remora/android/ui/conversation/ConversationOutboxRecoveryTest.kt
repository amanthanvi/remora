package com.remora.android.ui.conversation

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ConversationOutboxRecoveryTest {
    @Test
    fun uncertainDiscardRequiresMatchingRefreshCount() {
        assertFalse(isUncertainOutboxDiscardReady(currentCount = 1u, refreshedCount = null))
        assertFalse(isUncertainOutboxDiscardReady(currentCount = 2u, refreshedCount = 1u))
        assertFalse(isUncertainOutboxDiscardReady(currentCount = 0u, refreshedCount = 0u))
        assertTrue(isUncertainOutboxDiscardReady(currentCount = 2u, refreshedCount = 2u))
    }
}
