package com.zdroid

import kotlin.math.log10
import kotlin.math.sqrt

/**
 * Lightweight on-device VAD probe used to benchmark candidate-start latency before a model VAD
 * is selected. It never commits a turn; SpeechRecognizer remains the semantic authority.
 */
class MobileVadBenchmark(
    private val sampleRate: Int,
    private val onCandidateSpeech: (elapsedMs: Long, rmsDb: Double, thresholdDb: Double) -> Unit,
) {
    data class Snapshot(
        val frames: Long,
        val averageProcessingMicros: Long,
        val maxProcessingMicros: Long,
        val detections: Long,
    )

    private var noiseFloorDb = -55.0
    private var candidateSamples = 0L
    private var candidateReported = false
    private var frames = 0L
    private var totalProcessingNanos = 0L
    private var maxProcessingNanos = 0L
    private var detections = 0L

    @Synchronized
    fun processPcm16(buffer: ByteArray, count: Int) {
        val startedAt = System.nanoTime()
        if (count < 2) return
        var sumSquares = 0.0
        var samples = 0
        var index = 0
        while (index + 1 < count) {
            val value = ((buffer[index + 1].toInt() shl 8) or (buffer[index].toInt() and 0xff)).toShort().toDouble()
            sumSquares += value * value
            samples += 1
            index += 2
        }
        if (samples == 0) return
        val rms = sqrt(sumSquares / samples).coerceAtLeast(1.0)
        val rmsDb = 20.0 * log10(rms / Short.MAX_VALUE.toDouble())
        val threshold = (noiseFloorDb + SIGNAL_OVER_NOISE_DB).coerceIn(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB)
        if (rmsDb >= threshold) {
            candidateSamples += samples
            val elapsedMs = candidateSamples * 1_000L / sampleRate
            if (!candidateReported && elapsedMs >= MIN_CANDIDATE_MS) {
                candidateReported = true
                detections += 1
                onCandidateSpeech(elapsedMs, rmsDb, threshold)
            }
        } else {
            noiseFloorDb = noiseFloorDb * 0.96 + rmsDb * 0.04
            candidateSamples = 0L
            candidateReported = false
        }
        val processingNanos = System.nanoTime() - startedAt
        frames += 1
        totalProcessingNanos += processingNanos
        maxProcessingNanos = maxOf(maxProcessingNanos, processingNanos)
    }

    @Synchronized
    fun snapshot(): Snapshot = Snapshot(
        frames = frames,
        averageProcessingMicros = if (frames == 0L) 0L else totalProcessingNanos / frames / 1_000L,
        maxProcessingMicros = maxProcessingNanos / 1_000L,
        detections = detections,
    )

    @Synchronized
    fun reset() {
        candidateSamples = 0L
        candidateReported = false
    }

    companion object {
        private const val SIGNAL_OVER_NOISE_DB = 12.0
        private const val MIN_THRESHOLD_DB = -42.0
        private const val MAX_THRESHOLD_DB = -20.0
        private const val MIN_CANDIDATE_MS = 160L
    }
}
