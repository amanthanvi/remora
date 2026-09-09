package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.codex_mobile_client.ThreadKey

class AppModelNavigationIntentTest {
    private val old = ThreadKey("server", "old")
    private val selected = ThreadKey("server", "selected")

    @Test
    fun snapshotStartedBeforeCompletionCannotOverrideQueuedSelection() {
        val intent = AppModelNavigationIntent()
        val request = intent.request(selected)
        intent.beginDispatch(request)
        val staleFence = intent.snapshotFence()
        intent.completed(request)
        assertEquals(selected, intent.projectSnapshot(old, staleFence))
        assertEquals(selected, intent.projectSnapshot(selected, intent.snapshotFence()))
        assertEquals(old, intent.project(old))
    }

    @Test
    fun clearingSelectionIsAnIntentRatherThanNoPendingIntent() {
        val intent = AppModelNavigationIntent()
        val request = intent.request(null)
        intent.beginDispatch(request)
        assertNull(intent.project(old))
        assertNull(intent.projectSnapshot(old, intent.snapshotFence()))
        intent.completed(request)
        assertNull(intent.project(old))
        assertNull(intent.projectSnapshot(null, intent.snapshotFence()))
        assertEquals(old, intent.project(old))
    }

    @Test
    fun delayedFailureDoesNotClearNewerNavigation() {
        val intent = AppModelNavigationIntent()
        val first = intent.request(old)
        val second = intent.request(selected)
        intent.failed(first)
        assertEquals(selected, intent.project(old))
        intent.failed(second)
        assertEquals(old, intent.project(old))
    }

    @Test
    fun removedThreadCannotRemainThePendingSelection() {
        val intent = AppModelNavigationIntent()
        intent.request(selected)
        intent.removed(selected)
        assertNull(intent.project(null))
    }

    @Test
    fun repeatedSelectionCannotUseAnEarlierCompletionFence() {
        val intent = AppModelNavigationIntent()
        val first = intent.request(selected)
        intent.beginDispatch(first)
        intent.completed(first)
        val firstFence = intent.snapshotFence()
        val second = intent.request(old)
        intent.beginDispatch(second)
        intent.completed(second)
        val third = intent.request(selected)
        intent.beginDispatch(third)
        val secondFence = intent.snapshotFence()
        assertEquals(selected, intent.projectSnapshot(selected, firstFence))
        assertEquals(selected, intent.project(old))
        intent.completed(third)
        assertEquals(selected, intent.projectSnapshot(old, secondFence))
        assertEquals(selected, intent.project(old))
        intent.projectSnapshot(selected, intent.snapshotFence())
        assertEquals(old, intent.project(old))
    }

    @Test
    fun canonicalReadBeforeNativeCompletionDoesNotReleaseTheIntentEarly() {
        val intent = AppModelNavigationIntent()
        val request = intent.request(selected)
        intent.beginDispatch(request)
        intent.projectSnapshot(selected, intent.snapshotFence())
        assertEquals(selected, intent.project(old))
        intent.completed(request)
        assertEquals(selected, intent.project(old))
        intent.projectSnapshot(selected, intent.snapshotFence())
        assertEquals(old, intent.project(old))
    }

    @Test
    fun lostEchoIsRecoveredByAFreshSnapshotWithoutRestartingTheSubscription() {
        val intent = AppModelNavigationIntent()
        val request = intent.request(selected)
        intent.beginDispatch(request)
        val staleFence = intent.snapshotFence()
        intent.completed(request)
        assertEquals(selected, intent.projectSnapshot(selected, staleFence))
        assertEquals(selected, intent.project(old))
        assertEquals(selected, intent.projectSnapshot(selected, intent.snapshotFence()))
        assertEquals(old, intent.projectSnapshot(old, intent.snapshotFence()))
    }

    @Test
    fun twoCompletedSettersWithOnlyOneCoalescedRefreshReleaseTheLatestIntent() {
        val intent = AppModelNavigationIntent()
        val first = intent.request(old)
        intent.beginDispatch(first)
        intent.completed(first)
        val second = intent.request(selected)
        intent.beginDispatch(second)
        intent.completed(second)
        // Only B's activation event survives coalescing; it triggers one read.
        assertEquals(selected, intent.projectSnapshot(selected, intent.snapshotFence()))
        val canonicalThird = ThreadKey("server", "third")
        assertEquals(canonicalThird, intent.projectSnapshot(canonicalThird, intent.snapshotFence()))
    }

    @Test
    fun freshSnapshotCanObserveCanonicalNavigationAfterTheSetter() {
        val intent = AppModelNavigationIntent()
        val request = intent.request(selected)
        intent.beginDispatch(request)
        intent.completed(request)
        assertEquals(old, intent.projectSnapshot(old, intent.snapshotFence()))
        assertNull(intent.project(null))
    }

    @Test
    fun newerQueuedRequestIsNotReleasedByAnOlderSetterCompletion() {
        val intent = AppModelNavigationIntent()
        val first = intent.request(old)
        intent.beginDispatch(first)
        intent.request(selected)
        intent.completed(first)
        assertEquals(selected, intent.projectSnapshot(old, intent.snapshotFence()))
        assertEquals(false, intent.beginDispatch(first))
    }
}
