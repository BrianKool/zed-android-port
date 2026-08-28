package com.zdroid

import java.util.PriorityQueue

/** A stale-safe, interruptible speech queue with one platform utterance active at a time. */
class VoiceSpeechScheduler {
    enum class Priority(val weight: Int) { LOW(0), NORMAL(5), HIGH(10) }

    data class Speech(
        val id: String,
        val token: VoiceTurnManager.Token,
        val text: String,
        val priority: Priority,
        internal val sequence: Long,
        internal var spokenEnd: Int = 0,
    )

    data class Snapshot(
        val generatedText: String,
        val queuedText: String,
        val spokenText: String,
        val spokenCursor: Int,
    )

    private val queue = PriorityQueue<Speech>(
        compareByDescending<Speech> { it.priority.weight }.thenBy { it.sequence },
    )
    private val spoken = StringBuilder()
    private var generated = ""
    private var active: Speech? = null
    private var sequence = 0L

    @Synchronized
    fun updateGenerated(text: String) {
        generated = text
    }

    @Synchronized
    fun enqueue(
        token: VoiceTurnManager.Token,
        text: String,
        priority: Priority = Priority.NORMAL,
    ): Speech {
        val speech = Speech("zdroid-voice-${sequence}", token, text.trim(), priority, sequence++)
        queue.add(speech)
        return speech
    }

    @Synchronized
    fun shouldPreempt(priority: Priority): Boolean =
        active?.priority?.weight?.let { priority.weight > it } == true

    @Synchronized
    fun next(accepts: (VoiceTurnManager.Token) -> Boolean): Speech? {
        if (active != null) return null
        while (queue.isNotEmpty()) {
            val candidate = queue.remove()
            if (accepts(candidate.token) && candidate.text.isNotBlank()) {
                active = candidate
                return candidate
            }
        }
        return null
    }

    @Synchronized
    fun markRange(id: String, end: Int) {
        active?.takeIf { it.id == id }?.let {
            it.spokenEnd = end.coerceIn(it.spokenEnd, it.text.length)
        }
    }

    @Synchronized
    fun complete(id: String): Speech? {
        val item = active?.takeIf { it.id == id } ?: return null
        appendSpoken(item.text)
        active = null
        return item
    }

    @Synchronized
    fun fail(id: String): Speech? {
        val item = active?.takeIf { it.id == id } ?: return null
        active = null
        return item
    }

    /** Requeues only the unheard part of the active utterance. */
    @Synchronized
    fun pause() {
        active?.let { item ->
            if (item.spokenEnd > 0) appendSpoken(item.text.substring(0, item.spokenEnd))
            val remainder = item.text.substring(item.spokenEnd.coerceIn(0, item.text.length)).trim()
            if (remainder.isNotEmpty()) {
                queue.add(item.copy(id = "zdroid-voice-${sequence}", text = remainder, sequence = sequence++))
            }
        }
        active = null
    }

    @Synchronized
    fun interrupt() {
        active = null
        queue.clear()
    }

    @Synchronized
    fun hasPending(): Boolean = active != null || queue.isNotEmpty()

    @Synchronized
    fun snapshot(): Snapshot = Snapshot(
        generatedText = generated,
        queuedText = listOfNotNull(active?.text).plus(queue.map { it.text }).joinToString(" "),
        spokenText = spoken.toString(),
        spokenCursor = spoken.length.coerceAtMost(generated.length),
    )

    @Synchronized
    fun resetTurn() {
        generated = ""
        active = null
        queue.clear()
        spoken.clear()
    }

    private fun appendSpoken(value: String) {
        if (value.isBlank()) return
        if (spoken.isNotEmpty() && !spoken.last().isWhitespace()) spoken.append(' ')
        spoken.append(value.trim())
    }
}
