package com.zdroid

import android.content.Context
import android.os.SystemClock
import android.util.Log
import io.droidmcp.accessibility.GestureTool
import io.droidmcp.accessibility.GetActiveWindowInfoTool
import io.droidmcp.accessibility.GlobalActionTool
import io.droidmcp.apps.LaunchAppTool
import io.droidmcp.core.McpTool
import io.droidmcp.core.ParameterType
import io.droidmcp.core.ToolAnnotations
import io.droidmcp.core.ToolParameter
import io.droidmcp.core.ToolResult
import kotlinx.coroutines.delay
import org.json.JSONArray

/** Executes short deterministic Android action batches without repeated model round-trips. */
class PhoneActionPlanTool(context: Context) : McpTool {
    override val name = "execute_action_plan"
    override val description = "Execute a bounded sequence of deterministic Android actions locally. Use this for launch/wait/scroll/navigation chains; stop before any step that requires understanding screen content."
    override val parameters = listOf(
        ToolParameter(
            "plan_json",
            "JSON array (max 12) of actions: launch_app(package_name), wait_for_package(package_name, timeout_ms), scroll(direction: down|up|left|right), global_action(action), or delay(duration_ms).",
            ParameterType.STRING,
            required = true,
        ),
    )
    override val annotations = ToolAnnotations(destructiveHint = true, title = "Execute Phone Action Plan")

    private val launch = LaunchAppTool(context)
    private val activeWindow = GetActiveWindowInfoTool(context)
    private val gesture = GestureTool(context)
    private val global = GlobalActionTool(context)
    private val displayMetrics = context.resources.displayMetrics

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        val raw = params["plan_json"]?.toString()
            ?: return ToolResult.error("plan_json is required")
        val actions = runCatching { JSONArray(raw) }
            .getOrElse { return ToolResult.error("invalid_plan", it.message) }
        if (actions.length() !in 1..MAX_ACTIONS) {
            return ToolResult.error("invalid_plan", "plan must contain 1 to $MAX_ACTIONS actions")
        }

        val planStarted = SystemClock.elapsedRealtime()
        val results = mutableListOf<Map<String, Any?>>()
        for (index in 0 until actions.length()) {
            val action = actions.optJSONObject(index)
                ?: return ToolResult.error("invalid_plan", "action $index must be an object")
            val type = action.optString("type")
            val started = SystemClock.elapsedRealtime()
            val result = when (type) {
                "launch_app" -> action.nonEmptyString("package_name")?.let {
                    launch.execute(mapOf("package_name" to it))
                } ?: ToolResult.error("invalid_plan", "package_name is required")
                "wait_for_package" -> action.nonEmptyString("package_name")?.let {
                    waitForPackage(
                        it,
                        action.optLong("timeout_ms", DEFAULT_WAIT_MS).coerceIn(100L, MAX_WAIT_MS),
                    )
                } ?: ToolResult.error("invalid_plan", "package_name is required")
                "scroll" -> executeScroll(action.optString("direction", "down"))
                "global_action" -> {
                    val name = action.nonEmptyString("action")
                    if (name == null) {
                        ToolResult.error("invalid_plan", "action is required")
                    } else if (name !in ALLOWED_GLOBAL_ACTIONS) {
                        ToolResult.error("unsafe_action", "global action '$name' is not allowed in a batch")
                    } else {
                        global.execute(mapOf("action" to name))
                    }
                }
                "delay" -> {
                    val duration = action.optLong("duration_ms", 100L).coerceIn(0L, MAX_DELAY_MS)
                    delay(duration)
                    ToolResult.success(mapOf("duration_ms" to duration))
                }
                else -> ToolResult.error("unsupported_action", "action $index has unsupported type '$type'")
            }
            val duration = SystemClock.elapsedRealtime() - started
            Log.i(TAG, "plan action=$index type=$type success=${result.isSuccess} duration_ms=$duration")
            results += mapOf(
                "index" to index,
                "type" to type,
                "success" to result.isSuccess,
                "duration_ms" to duration,
                "data" to result.data,
                "error" to result.errorMessage,
            )
            if (!result.isSuccess) {
                return ToolResult.success(
                    mapOf(
                        "success" to false,
                        "failed_at" to index,
                        "total_duration_ms" to SystemClock.elapsedRealtime() - planStarted,
                        "steps" to results,
                    ),
                )
            }
        }
        return ToolResult.success(
            mapOf(
                "success" to true,
                "total_duration_ms" to SystemClock.elapsedRealtime() - planStarted,
                "steps" to results,
                "next" to "Use query_screen only if the next action requires semantic understanding.",
            ),
        )
    }

    private suspend fun waitForPackage(packageName: String, timeoutMs: Long): ToolResult {
        val started = SystemClock.elapsedRealtime()
        do {
            val result = activeWindow.execute(emptyMap())
            if (result.isSuccess && result.data?.get("package_name") == packageName) {
                return ToolResult.success(mapOf("package_name" to packageName))
            }
            delay(POLL_MS)
        } while (SystemClock.elapsedRealtime() - started < timeoutMs)
        return ToolResult.error("timeout", "foreground package did not become $packageName within ${timeoutMs}ms")
    }

    private suspend fun executeScroll(rawDirection: String): ToolResult {
        val width = displayMetrics.widthPixels.toFloat()
        val height = displayMetrics.heightPixels.toFloat()
        val points = actionPlanScrollPoints(rawDirection, width, height)
            ?: return ToolResult.error("invalid_plan", "scroll direction must be down|up|left|right")
        return gesture.execute(mapOf("points" to points, "duration_ms" to SCROLL_DURATION_MS))
    }

    private fun org.json.JSONObject.nonEmptyString(name: String): String? =
        optString(name).trim().takeIf(String::isNotEmpty)

    companion object {
        private const val TAG = "PhoneActionPlan"
        private const val MAX_ACTIONS = 12
        private const val DEFAULT_WAIT_MS = 3_000L
        private const val MAX_WAIT_MS = 5_000L
        private const val MAX_DELAY_MS = 1_000L
        private const val POLL_MS = 50L
        private const val SCROLL_DURATION_MS = 260L
        private val ALLOWED_GLOBAL_ACTIONS = setOf("back", "home", "recents")
    }
}

internal fun actionPlanScrollPoints(
    rawDirection: String,
    width: Float,
    height: Float,
): List<List<Float>>? {
    val direction = when (rawDirection.lowercase()) {
        "forward" -> "down"
        "back", "backward" -> "up"
        else -> rawDirection.lowercase()
    }
    val centerX = width * 0.5f
    val centerY = height * 0.5f
    return when (direction) {
        "down" -> listOf(listOf(centerX, height * 0.72f), listOf(centerX, height * 0.28f))
        "up" -> listOf(listOf(centerX, height * 0.28f), listOf(centerX, height * 0.72f))
        "right" -> listOf(listOf(width * 0.78f, centerY), listOf(width * 0.22f, centerY))
        "left" -> listOf(listOf(width * 0.22f, centerY), listOf(width * 0.78f, centerY))
        else -> null
    }
}
