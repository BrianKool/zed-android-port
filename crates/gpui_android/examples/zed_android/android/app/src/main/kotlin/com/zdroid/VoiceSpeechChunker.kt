package com.zdroid

/** Chooses natural streaming TTS boundaries without emitting incomplete words. */
object VoiceSpeechChunker {
    private const val MIN_CJK_CHARS = 12
    private const val MIN_LATIN_CHARS = 24
    private const val MIN_CJK_CLAUSE_CHARS = 8
    private const val MIN_LATIN_CLAUSE_CHARS = 12
    private const val MAX_CHARS = 140
    private val hardSentenceEndings = charArrayOf('。', '！', '？', '!', '?', '\n')
    private val clauseEndings = charArrayOf('，', '；', '：', ',', ';', ':')

    fun nextBoundary(text: CharSequence, force: Boolean): Int? {
        if (text.isEmpty()) return null

        val sentenceBoundary = firstSentenceBoundary(text)
        if (sentenceBoundary >= 0) return sentenceBoundary + 1
        if (force) return text.length

        val containsCjk = text.any { it.code in 0x3400..0x9FFF }
        val minimum = if (containsCjk) MIN_CJK_CLAUSE_CHARS else MIN_LATIN_CLAUSE_CHARS
        val clauseBoundary = text.indexOfFirst { index, character ->
            index + 1 >= minimum && character in clauseEndings
        }
        if (clauseBoundary >= 0) return clauseBoundary + 1

        if (containsCjk) {
            return if (text.length >= MIN_CJK_CHARS) {
                text.length.coerceAtMost(MAX_CHARS.coerceAtMost(MIN_CJK_CHARS * 2))
            } else {
                null
            }
        }

        if (text.length < MIN_LATIN_CHARS) return null
        val limit = text.length.coerceAtMost(MAX_CHARS)
        val wordBoundary = (limit - 1 downTo MIN_LATIN_CHARS - 1)
            .firstOrNull { text[it].isWhitespace() }
        return wordBoundary?.plus(1)
    }

    private fun firstSentenceBoundary(text: CharSequence): Int {
        text.forEachIndexed { index, character ->
            if (character in hardSentenceEndings) return index
            if (character == '.') {
                val previousIsDigit = index > 0 && text[index - 1].isDigit()
                val next = text.getOrNull(index + 1)
                val nextIsDigit = next?.isDigit() == true
                val endsSentence = next == null || next.isWhitespace()
                if (endsSentence && !(previousIsDigit && nextIsDigit)) return index
            }
        }
        return -1
    }

    private inline fun CharSequence.indexOfFirst(predicate: (Int, Char) -> Boolean): Int {
        for (index in indices) {
            if (predicate(index, this[index])) return index
        }
        return -1
    }
}
