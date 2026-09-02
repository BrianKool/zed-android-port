package com.zdroid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VoicePipelineTest {
    @Test
    fun orchestratorKeepsUserAgentAndPlaybackStatesOrthogonal() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        assertTrue(orchestrator.accept(VoiceOrchestrator.AgentEvent.Message(token, "hi")))
        assertTrue(orchestrator.playbackStarted(token))
        orchestrator.userSpeechStarted()

        val snapshot = orchestrator.snapshot()
        assertEquals(VoiceOrchestrator.UserState.SPEAKING, snapshot.user)
        assertEquals(VoiceOrchestrator.AgentState.RESPONDING, snapshot.agent)
        assertEquals(VoiceOrchestrator.PlaybackState.SPEAKING, snapshot.playback)
    }

    @Test
    fun staleAgentEventsAreRejectedByTurnAndRevision() {
        val orchestrator = VoiceOrchestrator()
        val old = orchestrator.commitTurn("first")
        orchestrator.cancelCurrentTurn()
        val current = orchestrator.commitTurn("second")

        assertFalse(orchestrator.accept(VoiceOrchestrator.AgentEvent.Message(old, "stale")))
        assertTrue(orchestrator.accept(VoiceOrchestrator.AgentEvent.Message(current, "current")))
    }

    @Test
    fun speechSchedulerPrioritizesAndPreservesUnheardSpeech() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        val scheduler = VoiceSpeechScheduler()
        scheduler.enqueue(token, "normal", VoiceSpeechScheduler.Priority.NORMAL)
        scheduler.enqueue(token, "urgent answer", VoiceSpeechScheduler.Priority.HIGH)

        val urgent = scheduler.next(orchestrator::accepts)!!
        assertEquals("urgent answer", urgent.text)
        scheduler.markRange(urgent.id, 7)
        scheduler.pause()
        val remainder = scheduler.next(orchestrator::accepts)!!
        assertEquals("answer", remainder.text)
        assertEquals("urgent", scheduler.snapshot().spokenText)
    }

    @Test
    fun highPrioritySpeechCanPreemptActiveNormalSpeech() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        val scheduler = VoiceSpeechScheduler()
        scheduler.enqueue(token, "normal response", VoiceSpeechScheduler.Priority.NORMAL)
        val normal = scheduler.next(orchestrator::accepts)!!
        scheduler.markRange(normal.id, 7)

        assertTrue(scheduler.shouldPreempt(VoiceSpeechScheduler.Priority.HIGH))
        scheduler.pause()
        scheduler.enqueue(token, "answer required", VoiceSpeechScheduler.Priority.HIGH)

        assertEquals("answer required", scheduler.next(orchestrator::accepts)?.text)
    }

    @Test
    fun completedAgentTurnStillAllowsAlreadyApprovedSpeech() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        assertTrue(orchestrator.accept(VoiceOrchestrator.AgentEvent.Finished(token)))
        assertTrue(orchestrator.accepts(token))
    }

    @Test
    fun falseInterruptionRestoresAgentFloorWithoutInvalidatingTurn() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        orchestrator.accept(VoiceOrchestrator.AgentEvent.Message(token, "answer"))
        orchestrator.playbackStarted(token)

        assertTrue(orchestrator.beginInterruption(true).isNotEmpty())
        orchestrator.playbackPaused()
        assertEquals(VoiceFloorManager.Floor.SHARED, orchestrator.snapshot().floor)
        assertTrue(orchestrator.recoverFalseInterruption().isNotEmpty())
        orchestrator.playbackResumed()

        assertTrue(orchestrator.accepts(token))
        assertEquals(VoiceFloorManager.Floor.AGENT, orchestrator.snapshot().floor)
    }

    @Test
    fun confirmedInterruptionCancelsOldSpeechToken() {
        val orchestrator = VoiceOrchestrator()
        val token = orchestrator.commitTurn("hello")
        orchestrator.accept(VoiceOrchestrator.AgentEvent.Message(token, "answer"))
        orchestrator.beginInterruption(true)

        assertTrue(orchestrator.confirmInterruption().isNotEmpty())
        orchestrator.cancelCurrentTurn()
        assertFalse(orchestrator.accepts(token))
    }

    @Test
    fun speechPolicyDropsPrivateAndMachineOrientedEvents() {
        val token = VoiceTurnManager.Token(1, 1)
        assertFalse(VoiceSpeechPolicy.shouldSpeak(VoiceOrchestrator.AgentEvent.Thinking(token)))
        assertFalse(VoiceSpeechPolicy.shouldSpeak(VoiceOrchestrator.AgentEvent.ToolStarted(token, "test")))
        assertEquals(
            "I checked it. Done.",
            VoiceSpeechPolicy.prepare(
                VoiceOrchestrator.AgentEvent.Message(
                    token,
                    "<thinking>secret</thinking>I checked it. ```kotlin\nval x = 1\n``` Done.",
                ),
            ),
        )
    }

    @Test
    fun speechPolicyWaitsWhenPrivateReasoningOrCodeIsStillStreaming() {
        val token = VoiceTurnManager.Token(1, 1)
        assertEquals(
            "",
            VoiceSpeechPolicy.prepare(
                VoiceOrchestrator.AgentEvent.Message(token, "<analysis>still deciding"),
            ),
        )
        assertEquals(
            "I found the issue.",
            VoiceSpeechPolicy.prepare(
                VoiceOrchestrator.AgentEvent.Message(
                    token,
                    "<reasoning>private</reasoning>I found the issue. ```kotlin\nval pending = true",
                ),
            ),
        )
    }

    @Test
    fun speechPolicyKeepsPublicStatusButDropsToolEvents() {
        val token = VoiceTurnManager.Token(1, 1)
        assertEquals(
            "I am checking the project.",
            VoiceSpeechPolicy.prepare(
                VoiceOrchestrator.AgentEvent.Status(token, "I am checking the project."),
            ),
        )
        assertEquals(
            "",
            VoiceSpeechPolicy.prepare(
                VoiceOrchestrator.AgentEvent.ToolStarted(token, "Read settings.json"),
            ),
        )
    }

    @Test
    fun endpointingLearnsSessionPauseCadenceWithinBounds() {
        val endpointing = DynamicEndpointing()
        endpointing.speechEnded(1_000)
        endpointing.speechResumed(1_500)
        endpointing.speechEnded(2_000)
        endpointing.speechResumed(2_600)

        assertEquals(690L, endpointing.timeoutMs())
    }

    @Test
    fun vadRequiresSustainedSignalAboveAdaptiveNoiseFloor() {
        var detections = 0
        val detector = MobileVadBenchmark(16_000) { _, _, _ -> detections += 1 }
        val silence = pcm(amplitude = 30, samples = 3_200)
        val voice = pcm(amplitude = 12_000, samples = 3_200)

        detector.processPcm16(silence, silence.size)
        assertEquals(0, detections)
        detector.processPcm16(voice, voice.size)
        assertEquals(1, detections)
    }

    private fun pcm(amplitude: Int, samples: Int): ByteArray = ByteArray(samples * 2).also { bytes ->
        repeat(samples) { index ->
            val sample = if (index % 2 == 0) amplitude else -amplitude
            bytes[index * 2] = (sample and 0xff).toByte()
            bytes[index * 2 + 1] = ((sample shr 8) and 0xff).toByte()
        }
    }
}
