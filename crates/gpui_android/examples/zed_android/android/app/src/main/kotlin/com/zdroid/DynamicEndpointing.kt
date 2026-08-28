package com.zdroid

/** Learns within-session pause cadence while retaining conservative latency bounds. */
class DynamicEndpointing {
    private val withinTurnPauses = ArrayDeque<Long>()
    private var speechEndedAt = 0L

    @Synchronized
    fun speechEnded(at: Long) {
        speechEndedAt = at
    }

    @Synchronized
    fun speechResumed(at: Long) {
        if (speechEndedAt <= 0L) return
        val pause = (at - speechEndedAt).coerceAtLeast(0L)
        if (pause in MIN_OBSERVED_PAUSE_MS..MAX_OBSERVED_PAUSE_MS) {
            withinTurnPauses.addLast(pause)
            while (withinTurnPauses.size > HISTORY_SIZE) withinTurnPauses.removeFirst()
        }
        speechEndedAt = 0L
    }

    @Synchronized
    fun timeoutMs(): Long {
        if (withinTurnPauses.isEmpty()) return DEFAULT_TIMEOUT_MS
        val sorted = withinTurnPauses.sorted()
        val median = sorted[sorted.size / 2]
        return (median * 1.15).toLong().coerceIn(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
    }

    @Synchronized
    fun reset() {
        withinTurnPauses.clear()
        speechEndedAt = 0L
    }

    companion object {
        private const val HISTORY_SIZE = 12
        private const val MIN_OBSERVED_PAUSE_MS = 120L
        private const val MAX_OBSERVED_PAUSE_MS = 2_000L
        private const val DEFAULT_TIMEOUT_MS = 500L
        private const val MIN_TIMEOUT_MS = 300L
        private const val MAX_TIMEOUT_MS = 1_200L
    }
}
