package com.zdroid

import android.content.SharedPreferences

internal object VoicePreferences {
    const val INPUT_LANGUAGE_AUTO = "auto"
    const val INPUT_LANGUAGE_TRADITIONAL_CHINESE = "zh-TW"
    const val INPUT_LANGUAGE_ENGLISH = "en-US"
    const val DEFAULT_SPEAKER_ENABLED = true

    fun speakerEnabled(preferences: SharedPreferences): Boolean =
        preferences.getBoolean("speaker", DEFAULT_SPEAKER_ENABLED)

    fun inputLanguage(preferences: SharedPreferences): String {
        preferences.getString("input_language", null)?.let { saved ->
            if (saved in supportedInputLanguages) return saved
        }

        val legacy = preferences.getStringSet("languages", emptySet()).orEmpty()
        val migrated = when (legacy) {
            setOf(INPUT_LANGUAGE_TRADITIONAL_CHINESE) -> INPUT_LANGUAGE_TRADITIONAL_CHINESE
            setOf(INPUT_LANGUAGE_ENGLISH) -> INPUT_LANGUAGE_ENGLISH
            else -> INPUT_LANGUAGE_AUTO
        }
        preferences.edit()
            .putString("input_language", migrated)
            .remove("languages")
            .apply()
        return migrated
    }

    fun recognitionLanguages(inputLanguage: String): List<String> = when (inputLanguage) {
        INPUT_LANGUAGE_TRADITIONAL_CHINESE -> listOf(INPUT_LANGUAGE_TRADITIONAL_CHINESE)
        INPUT_LANGUAGE_ENGLISH -> listOf(INPUT_LANGUAGE_ENGLISH)
        else -> listOf(INPUT_LANGUAGE_TRADITIONAL_CHINESE, INPUT_LANGUAGE_ENGLISH)
    }

    private val supportedInputLanguages = setOf(
        INPUT_LANGUAGE_AUTO,
        INPUT_LANGUAGE_TRADITIONAL_CHINESE,
        INPUT_LANGUAGE_ENGLISH,
    )
}
