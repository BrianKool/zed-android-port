package com.zdroid

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VoiceTranscriptStabilityTest {
    @Test
    fun singlePartialBecomesStableAfterEndpointWindow() {
        assertFalse(isStablePartialTranscript(1, 2, 449L, 450L))
        assertTrue(isStablePartialTranscript(1, 2, 450L, 450L))
    }

    @Test
    fun repeatedPartialCanFinalizeAtEndpoint() {
        assertTrue(isStablePartialTranscript(2, 2, 100L, 450L))
    }
}
