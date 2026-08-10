package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CurrentWorkspaceRebuildTest {
    @Test
    fun rebuildClearsOnlyObsoleteProductStateBeforeCommittingMarker() {
        val backend = FakeBackend()

        assertEquals(
            CurrentWorkspaceRebuild.Result(completed = true, didRebuild = true),
            CurrentWorkspaceRebuild.apply(backend),
        )
        assertEquals(listOf("productState", "features", "marker"), backend.operations)
        assertTrue(backend.complete)
    }

    @Test
    fun completedMarkerMakesRebuildIdempotent() {
        val backend = FakeBackend(complete = true)

        assertEquals(
            CurrentWorkspaceRebuild.Result(completed = true, didRebuild = false),
            CurrentWorkspaceRebuild.apply(backend),
        )
        assertTrue(backend.operations.isEmpty())
    }

    @Test
    fun cleanupFailureNeverTouchesFeaturesOrCommitsMarker() {
        val backend = FakeBackend(productStateResult = false)

        assertFalse(CurrentWorkspaceRebuild.apply(backend).completed)
        assertEquals(listOf("productState"), backend.operations)
        assertFalse(backend.complete)
    }

    @Test
    fun featureFailureNeverCommitsMarker() {
        val backend = FakeBackend(featureResult = false)

        assertFalse(CurrentWorkspaceRebuild.apply(backend).completed)
        assertEquals(listOf("productState", "features"), backend.operations)
        assertFalse(backend.complete)
    }

    @Test
    fun markerFailureLeavesRebuildIncomplete() {
        val backend = FakeBackend(markerResult = false)

        assertFalse(CurrentWorkspaceRebuild.apply(backend).completed)
        assertEquals(listOf("productState", "features", "marker"), backend.operations)
        assertFalse(backend.complete)
    }

    private class FakeBackend(
        var complete: Boolean = false,
        private val productStateResult: Boolean = true,
        private val featureResult: Boolean = true,
        private val markerResult: Boolean = true,
    ) : WorkspaceRebuildBackend {
        val operations = mutableListOf<String>()

        override fun isComplete(): Boolean = complete

        override fun clearObsoleteProductState(): Boolean {
            operations += "productState"
            return productStateResult
        }

        override fun removeRetiredFeatureOverrides(): Boolean {
            operations += "features"
            return featureResult
        }

        override fun markComplete(): Boolean {
            operations += "marker"
            if (markerResult) complete = true
            return markerResult
        }
    }
}
