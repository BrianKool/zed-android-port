package com.zdroid

import android.os.SystemClock
import android.util.Log

/** Per-turn monotonic timestamps for voice latency and interruption diagnostics. */
class VoiceLatencyTelemetry(private val tag: String) {
    private val marks = LinkedHashMap<String, Long>()

    @Synchronized
    fun mark(name: String, at: Long = SystemClock.elapsedRealtime()) {
        marks.putIfAbsent(name, at)
    }

    @Synchronized
    fun replace(name: String, at: Long = SystemClock.elapsedRealtime()) {
        marks[name] = at
    }

    @Synchronized
    fun duration(from: String, to: String): Long? {
        val start = marks[from] ?: return null
        val end = marks[to] ?: return null
        return (end - start).coerceAtLeast(0L)
    }

    @Synchronized
    fun log(label: String, vararg pairs: Pair<String, String>) {
        val values = pairs.mapNotNull { (name, expression) ->
            val parts = expression.split("->", limit = 2)
            if (parts.size != 2) null else duration(parts[0], parts[1])?.let { "$name=${it}ms" }
        }
        Log.i(tag, "latency $label ${values.joinToString(" ")}")
    }

    @Synchronized
    fun reset() = marks.clear()
}
