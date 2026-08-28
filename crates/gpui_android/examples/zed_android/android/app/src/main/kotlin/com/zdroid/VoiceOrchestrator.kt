package com.zdroid

/** Coordinates independent conversation state without owning Android audio APIs. */
class VoiceOrchestrator {
    enum class UserState { SILENT, SPEAKING }
    enum class AgentState { IDLE, THINKING, TOOL_RUNNING, WAITING_FOR_USER, RESPONDING }
    enum class PlaybackState { IDLE, SPEAKING, PAUSED }

    data class Snapshot(
        val user: UserState,
        val agent: AgentState,
        val playback: PlaybackState,
        val floor: VoiceFloorManager.Floor,
        val interruption: VoiceInterruptionArbiter.State,
        val turn: VoiceTurnManager.Token?,
        val phase: VoiceTurnManager.Phase?,
    )

    sealed interface AgentEvent {
        val token: VoiceTurnManager.Token
        data class Message(override val token: VoiceTurnManager.Token, val text: String) : AgentEvent
        data class Thinking(override val token: VoiceTurnManager.Token) : AgentEvent
        data class ToolStarted(override val token: VoiceTurnManager.Token, val label: String) : AgentEvent
        data class ToolFinished(override val token: VoiceTurnManager.Token, val label: String) : AgentEvent
        data class WaitingForUser(override val token: VoiceTurnManager.Token, val text: String) : AgentEvent
        data class Status(override val token: VoiceTurnManager.Token, val text: String) : AgentEvent
        data class Finished(override val token: VoiceTurnManager.Token) : AgentEvent
        data class Failed(override val token: VoiceTurnManager.Token, val text: String) : AgentEvent
    }

    private val turns = VoiceTurnManager()
    private val floors = VoiceFloorManager()
    private val interruptions = VoiceInterruptionArbiter()
    private var userState = UserState.SILENT
    private var agentState = AgentState.IDLE
    private var playbackState = PlaybackState.IDLE

    @Synchronized fun userSpeechStarted() { userState = UserState.SPEAKING }
    @Synchronized fun updateProvisionalTranscript(text: String) = turns.updateProvisional(text)
    @Synchronized fun commitTurn(text: String): VoiceTurnManager.Token {
        userState = UserState.SILENT
        agentState = AgentState.THINKING
        interruptions.reset()
        return turns.commit(text)
    }
    @Synchronized fun cancelCurrentTurn() { turns.cancel(); agentState = AgentState.IDLE }
    @Synchronized fun currentToken() = turns.token()
    @Synchronized fun accepts(token: VoiceTurnManager.Token) = turns.acceptsPlayback(token)

    @Synchronized fun beginInterruption(hasAgentOutput: Boolean): Set<VoiceConversationPolicy.Action> {
        userState = UserState.SPEAKING
        return VoiceConversationPolicy.candidate(interruptions.begin(hasAgentOutput))
    }
    @Synchronized fun confirmInterruption() = VoiceConversationPolicy.confirmed(interruptions.confirm())
    @Synchronized fun recoverFalseInterruption(): Set<VoiceConversationPolicy.Action> {
        val actions = VoiceConversationPolicy.falseAlarm(interruptions.recoverFalseInterruption())
        if (actions.isNotEmpty()) userState = UserState.SILENT
        return actions
    }

    @Synchronized fun accept(event: AgentEvent): Boolean {
        if (!turns.acceptsEvent(event.token)) return false
        agentState = when (event) {
            is AgentEvent.Message -> AgentState.RESPONDING
            is AgentEvent.Thinking -> AgentState.THINKING
            is AgentEvent.ToolStarted -> AgentState.TOOL_RUNNING
            is AgentEvent.ToolFinished -> AgentState.THINKING
            is AgentEvent.WaitingForUser -> AgentState.WAITING_FOR_USER
            is AgentEvent.Status -> agentState
            is AgentEvent.Finished, is AgentEvent.Failed -> AgentState.IDLE
        }
        if (event is AgentEvent.Finished || event is AgentEvent.Failed) turns.complete(event.token)
        return true
    }

    @Synchronized fun playbackStarted(token: VoiceTurnManager.Token): Boolean {
        if (!turns.acceptsPlayback(token)) return false
        playbackState = PlaybackState.SPEAKING
        return true
    }
    @Synchronized fun playbackPaused() { playbackState = PlaybackState.PAUSED }
    @Synchronized fun playbackResumed() { playbackState = PlaybackState.SPEAKING }
    @Synchronized fun playbackFinished() { playbackState = PlaybackState.IDLE }

    @Synchronized fun snapshot() = Snapshot(
        userState,
        agentState,
        playbackState,
        floors.resolve(userState, agentState, playbackState),
        interruptions.state,
        turns.token(),
        turns.phase(),
    )

    @Synchronized fun reset() {
        turns.reset()
        interruptions.reset()
        userState = UserState.SILENT
        agentState = AgentState.IDLE
        playbackState = PlaybackState.IDLE
    }
}
