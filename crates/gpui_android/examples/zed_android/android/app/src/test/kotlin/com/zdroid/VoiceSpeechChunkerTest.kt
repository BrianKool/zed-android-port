package com.zdroid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class VoiceSpeechChunkerTest {
    @Test
    fun doesNotSplitShortEnglishContractions() {
        assertNull(VoiceSpeechChunker.nextBoundary("I", force = false))
        assertNull(VoiceSpeechChunker.nextBoundary("I'll", force = false))
    }

    @Test
    fun emitsCompleteSentencesImmediately() {
        assertEquals(15, VoiceSpeechChunker.nextBoundary("I'll check now.", force = false))
        assertEquals(6, VoiceSpeechChunker.nextBoundary("我先查一下。接著處理", force = false))
    }

    @Test
    fun longEnglishChunksEndAtAWordBoundary() {
        val text = "I'll look up tomorrow's Sydney weather and report back shortly"
        val boundary = VoiceSpeechChunker.nextBoundary(text, force = false)

        requireNotNull(boundary)
        assertEquals(' ', text[boundary - 1])
    }

    @Test
    fun completionFlushesRemainingText() {
        assertEquals(4, VoiceSpeechChunker.nextBoundary("I'll", force = true))
    }

    @Test
    fun doesNotTreatUrlOrDecimalDotsAsSentenceEnds() {
        assertNull(VoiceSpeechChunker.nextBoundary("Open example.com", force = false))
        assertNull(VoiceSpeechChunker.nextBoundary("The value is 3.14", force = false))
    }

    @Test
    fun emitsStableClausesWithoutWaitingForTheWholeAnswer() {
        assertEquals(14, VoiceSpeechChunker.nextBoundary("I'll check it, then report back", force = false))
        assertEquals(10, VoiceSpeechChunker.nextBoundary("我先檢查目前的設定，接著處理", force = false))
    }
}
