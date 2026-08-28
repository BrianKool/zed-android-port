package com.zdroid

import java.util.concurrent.atomic.AtomicLong

/** Owns turn identity and revision; it contains no audio or UI state. */
class VoiceTurnManager {
    enum class Phase { PROVISIONAL, COMMITTED, CANCELLED, COMPLETED }
    data class Token(val id: Long, val revision: Long)
    private data class Turn(val id: Long, var revision: Long, var transcript: String, var phase: Phase)
    private val ids = AtomicLong(0L)
    private var current: Turn? = null

    @Synchronized fun updateProvisional(text: String): Token {
        val turn = current?.takeIf { it.phase == Phase.PROVISIONAL }
            ?: Turn(ids.incrementAndGet(), 0L, "", Phase.PROVISIONAL).also { current = it }
        updateTranscript(turn, text)
        return turn.token()
    }

    @Synchronized fun commit(text: String): Token {
        current?.takeIf { it.phase == Phase.COMMITTED && it.transcript == text }?.let { return it.token() }
        val turn = current?.takeIf { it.phase == Phase.PROVISIONAL }
            ?: Turn(ids.incrementAndGet(), 0L, "", Phase.PROVISIONAL).also { current = it }
        updateTranscript(turn, text)
        turn.phase = Phase.COMMITTED
        return turn.token()
    }

    @Synchronized fun cancel() { current?.phase = Phase.CANCELLED }
    @Synchronized fun complete(token: Token): Boolean {
        if (!acceptsEvent(token)) return false
        current?.phase = Phase.COMPLETED
        return true
    }
    @Synchronized fun token(): Token? = current?.token()
    @Synchronized fun acceptsPlayback(token: Token): Boolean = matches(token) && current?.phase != Phase.CANCELLED
    @Synchronized fun acceptsEvent(token: Token): Boolean = matches(token) && current?.phase == Phase.COMMITTED
    @Synchronized fun phase(): Phase? = current?.phase
    @Synchronized fun reset() { current = null }

    private fun matches(token: Token) = current?.let { it.id == token.id && it.revision == token.revision } == true
    private fun updateTranscript(turn: Turn, text: String) {
        if (text != turn.transcript) { turn.revision += 1L; turn.transcript = text }
    }
    private fun Turn.token() = Token(id, revision)
}
