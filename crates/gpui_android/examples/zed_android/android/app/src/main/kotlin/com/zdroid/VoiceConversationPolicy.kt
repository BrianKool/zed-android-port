package com.zdroid

class VoiceFloorManager {
    enum class Floor { USER, AGENT, SHARED, UNDECIDED }
    fun resolve(user: VoiceOrchestrator.UserState, agent: VoiceOrchestrator.AgentState, playback: VoiceOrchestrator.PlaybackState): Floor {
        val userActive = user == VoiceOrchestrator.UserState.SPEAKING
        val agentActive = agent != VoiceOrchestrator.AgentState.IDLE || playback != VoiceOrchestrator.PlaybackState.IDLE
        return when { userActive && agentActive -> Floor.SHARED; userActive -> Floor.USER; agentActive -> Floor.AGENT; else -> Floor.UNDECIDED }
    }
}

class VoiceInterruptionArbiter {
    enum class State { IDLE, CANDIDATE, CONFIRMED }
    var state = State.IDLE
        private set
    @Synchronized fun begin(hasAgentOutput: Boolean): Boolean {
        if (!hasAgentOutput || state != State.IDLE) return false
        state = State.CANDIDATE
        return true
    }
    @Synchronized fun confirm(): Boolean {
        if (state != State.CANDIDATE) return false
        state = State.CONFIRMED
        return true
    }
    @Synchronized fun recoverFalseInterruption(): Boolean {
        if (state != State.CANDIDATE) return false
        state = State.IDLE
        return true
    }
    @Synchronized fun reset() { state = State.IDLE }
}

object VoiceConversationPolicy {
    enum class Action { PAUSE_PLAYBACK, RESUME_PLAYBACK, STOP_PLAYBACK, CANCEL_AGENT }
    fun candidate(started: Boolean) = if (started) setOf(Action.PAUSE_PLAYBACK) else emptySet()
    fun confirmed(confirmed: Boolean) = if (confirmed) setOf(Action.STOP_PLAYBACK, Action.CANCEL_AGENT) else emptySet()
    fun falseAlarm(recovered: Boolean) = if (recovered) setOf(Action.RESUME_PLAYBACK) else emptySet()
}
