package com.zdroid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class VoicePreferencesTest {
    @Test
    fun speakerIsTheDefaultAudioOutput() {
        assertTrue(VoicePreferences.DEFAULT_SPEAKER_ENABLED)
    }

    @Test
    fun autoDetectOffersBothSupportedRecognitionLanguages() {
        assertEquals(
            listOf("zh-TW", "en-US"),
            VoicePreferences.recognitionLanguages(VoicePreferences.INPUT_LANGUAGE_AUTO),
        )
    }

    @Test
    fun explicitRecognitionLanguageIsExclusive() {
        assertEquals(
            listOf("zh-TW"),
            VoicePreferences.recognitionLanguages(
                VoicePreferences.INPUT_LANGUAGE_TRADITIONAL_CHINESE,
            ),
        )
        assertEquals(
            listOf("en-US"),
            VoicePreferences.recognitionLanguages(VoicePreferences.INPUT_LANGUAGE_ENGLISH),
        )
    }
}
