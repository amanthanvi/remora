package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ActiveTerminalRegistryTest {
    @Test
    fun freshSelectionIsDeliveredOnlyAfterSourceCallback() {
        var sourceCallback: ((String?) -> Unit)? = null
        val source = ActiveTerminalRegistry.SelectionSource { callback ->
            sourceCallback = callback
        }
        var result: String? = "stale"

        dispatchFreshSelection(source) { result = it }

        assertEquals("stale", result)
        sourceCallback?.invoke("fresh selection")
        assertEquals("fresh selection", result)
    }

    @Test
    fun emptyOrMissingFreshSelectionIsNull() {
        var emptyResult: String? = "stale"
        dispatchFreshSelection(
            ActiveTerminalRegistry.SelectionSource { callback -> callback("") },
        ) { emptyResult = it }
        assertNull(emptyResult)

        var missingResult: String? = "stale"
        dispatchFreshSelection(null) { missingResult = it }
        assertNull(missingResult)
    }
}
