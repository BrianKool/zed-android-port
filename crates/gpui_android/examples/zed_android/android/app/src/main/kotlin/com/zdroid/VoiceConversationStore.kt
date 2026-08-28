package com.zdroid

/** A bounded presentation mirror of the Agent thread used by the call surface. */
object VoiceConversationStore {
    enum class Role { USER, ASSISTANT }

    data class Bubble(
        val id: Long,
        val role: Role,
        val text: String,
        val partial: Boolean = false,
        val streaming: Boolean = false,
    )

    private val bubbles = ArrayDeque<Bubble>()
    private var nextId = 1L
    private var threadId = ""

    @Synchronized
    fun begin(threadId: String) {
        if (this.threadId != threadId) {
            this.threadId = threadId
            bubbles.clear()
        }
    }

    @Synchronized
    fun setPartial(text: String) {
        removePartialLocked()
        if (text.isNotBlank()) {
            bubbles.addLast(Bubble(nextId++, Role.USER, text, partial = true))
        }
        trimLocked()
    }

    @Synchronized
    fun commitUser(text: String) {
        removePartialLocked()
        if (text.isNotBlank()) bubbles.addLast(Bubble(nextId++, Role.USER, text))
        trimLocked()
    }

    @Synchronized
    fun updateAssistant(text: String) {
        if (text.isBlank()) return
        val last = bubbles.lastOrNull()
        if (last?.role == Role.ASSISTANT && last.streaming) {
            bubbles.removeLast()
            bubbles.addLast(last.copy(text = text))
        } else {
            bubbles.addLast(Bubble(nextId++, Role.ASSISTANT, text, streaming = true))
        }
        trimLocked()
    }

    @Synchronized
    fun finishAssistant() {
        val last = bubbles.lastOrNull() ?: return
        if (last.role == Role.ASSISTANT && last.streaming) {
            bubbles.removeLast()
            bubbles.addLast(last.copy(streaming = false))
        }
    }

    @Synchronized
    fun snapshot(): List<Bubble> = bubbles.toList()

    private fun removePartialLocked() {
        if (bubbles.lastOrNull()?.partial == true) bubbles.removeLast()
    }

    private fun trimLocked() {
        while (bubbles.size > MAX_BUBBLES || bubbles.sumOf { it.text.length } > MAX_CHARACTERS) {
            bubbles.removeFirstOrNull()
        }
    }

    private const val MAX_BUBBLES = 200
    private const val MAX_CHARACTERS = 120_000
}
