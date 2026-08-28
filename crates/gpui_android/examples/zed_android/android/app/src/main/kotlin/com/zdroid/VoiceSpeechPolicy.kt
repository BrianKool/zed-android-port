package com.zdroid

/** Converts visible Agent output into text that is safe and useful to read aloud. */
object VoiceSpeechPolicy {
    fun shouldSpeak(event: VoiceOrchestrator.AgentEvent): Boolean = when (event) {
        is VoiceOrchestrator.AgentEvent.Message,
        is VoiceOrchestrator.AgentEvent.WaitingForUser,
        is VoiceOrchestrator.AgentEvent.Status,
        is VoiceOrchestrator.AgentEvent.Failed -> true
        is VoiceOrchestrator.AgentEvent.Thinking,
        is VoiceOrchestrator.AgentEvent.ToolStarted,
        is VoiceOrchestrator.AgentEvent.ToolFinished,
        is VoiceOrchestrator.AgentEvent.Finished -> false
    }

    fun prepare(event: VoiceOrchestrator.AgentEvent): String {
        if (!shouldSpeak(event)) return ""
        val text = when (event) {
            is VoiceOrchestrator.AgentEvent.Message -> event.text
            is VoiceOrchestrator.AgentEvent.WaitingForUser -> event.text
            is VoiceOrchestrator.AgentEvent.Status -> event.text
            is VoiceOrchestrator.AgentEvent.Failed -> event.text
            else -> ""
        }
        return text
            .replace(THINKING_BLOCK, " ")
            .replace(CODE_BLOCK, " ")
            .replace(TOOL_MARKUP, " ")
            .replace(INLINE_CODE, "$1")
            .replace(Regex("[\\t ]+"), " ")
            .replace(Regex("\\n{3,}"), "\n\n")
            .trim()
    }

    fun priority(event: VoiceOrchestrator.AgentEvent): VoiceSpeechScheduler.Priority = when (event) {
        is VoiceOrchestrator.AgentEvent.WaitingForUser,
        is VoiceOrchestrator.AgentEvent.Failed -> VoiceSpeechScheduler.Priority.HIGH
        is VoiceOrchestrator.AgentEvent.Status -> VoiceSpeechScheduler.Priority.LOW
        else -> VoiceSpeechScheduler.Priority.NORMAL
    }

    private val THINKING_BLOCK = Regex("(?is)<thinking>.*?(?:</thinking>|$)")
    private val CODE_BLOCK = Regex("(?s)```.*?(?:```|$)")
    private val TOOL_MARKUP = Regex(
        "(?is)<(tool_call|tool_result)[^>]*>.*?(?:</\\1>|$)",
    )
    private val INLINE_CODE = Regex("`([^`]+)`")
}
