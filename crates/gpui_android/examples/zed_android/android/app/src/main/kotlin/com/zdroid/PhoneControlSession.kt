package com.zdroid

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.Context
import android.content.Intent
import android.graphics.Path
import android.os.SystemClock
import android.util.Log
import io.droidmcp.accessibility.AccessibilityServiceHolder
import java.util.Locale

/** Session-aware local command path for safe, high-frequency Phone Use actions. */
object PhoneControlSession {
    data class ContextSnapshot(
        val currentPackage: String?,
        val lastAction: Action?,
        val lastDirection: Direction?,
        val updatedAtMs: Long,
    )

    sealed interface Action {
        data class Scroll(val direction: Direction) : Action
        data class Global(val action: Int, val label: String) : Action
        data class Launch(val packageName: String, val label: String) : Action
    }

    enum class Direction { DOWN, UP, LEFT, RIGHT }

    data class Result(val handled: Boolean, val message: String = "")

    @Volatile private var snapshot = ContextSnapshot(null, null, null, 0L)

    fun resolveAndExecute(context: Context, transcript: String): Result {
        val commands = splitCommands(normalize(transcript))
        if (commands.isEmpty()) return Result(false)

        var handledAny = false
        var lastMessage = ""
        for (command in commands) {
            val action = resolve(context, command) ?: return if (handledAny) {
                Result(true, lastMessage)
            } else {
                Result(false)
            }
            val result = execute(context, action)
            if (!result.handled) return result
            handledAny = true
            lastMessage = result.message
        }
        return Result(handledAny, lastMessage)
    }

    fun contextSnapshot(): ContextSnapshot = snapshot

    private fun resolve(context: Context, command: String): Action? {
        directionForCommand(command)?.let { return Action.Scroll(it) }
        return when {
            command in BACK -> Action.Global(AccessibilityService.GLOBAL_ACTION_BACK, "Back")
            command in HOME -> Action.Global(AccessibilityService.GLOBAL_ACTION_HOME, "Home")
            command in REPEAT -> snapshot.lastAction?.takeIf(::isRepeatable)
            command.startsWith("打開") -> resolveLaunch(context, command.removePrefix("打開").trim())
            command.startsWith("open") -> resolveLaunch(context, command.removePrefix("open").trim())
            else -> null
        }
    }

    private fun resolveLaunch(context: Context, requestedName: String): Action.Launch? {
        if (requestedName.isBlank()) return null
        val packageManager = context.packageManager
        val normalizedName = normalize(requestedName)
        val matches = packageManager.getInstalledApplications(0).mapNotNull { app ->
            val launchIntent = packageManager.getLaunchIntentForPackage(app.packageName) ?: return@mapNotNull null
            val label = packageManager.getApplicationLabel(app).toString()
            val normalizedLabel = normalize(label)
            if (normalizedLabel == normalizedName || normalizedLabel.contains(normalizedName)) {
                Triple(app.packageName, label, launchIntent)
            } else {
                null
            }
        }
        val match = matches.singleOrNull() ?: return null
        return Action.Launch(match.first, match.second)
    }

    private fun execute(context: Context, action: Action): Result {
        val service = AccessibilityServiceHolder.service
        val result = when (action) {
            is Action.Scroll -> {
                if (service == null) return Result(false)
                val metrics = context.resources.displayMetrics
                val points = actionPlanScrollPoints(
                    action.direction.name.lowercase(Locale.ROOT),
                    metrics.widthPixels.toFloat(),
                    metrics.heightPixels.toFloat(),
                ) ?: return Result(false)
                val path = Path().apply {
                    moveTo(points[0][0], points[0][1])
                    lineTo(points[1][0], points[1][1])
                }
                val dispatched = service.dispatchGesture(
                    GestureDescription.Builder()
                        .addStroke(GestureDescription.StrokeDescription(path, 0L, 260L))
                        .build(),
                    null,
                    null,
                )
                Result(dispatched, if (dispatched) "Scrolled ${action.direction.name.lowercase()}" else "")
            }
            is Action.Global -> {
                if (service == null) return Result(false)
                val performed = service.performGlobalAction(action.action)
                Result(performed, if (performed) action.label else "")
            }
            is Action.Launch -> {
                val launchIntent = context.packageManager.getLaunchIntentForPackage(action.packageName)
                    ?: return Result(false)
                context.startActivity(launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                Result(true, "Opened ${action.label}")
            }
        }
        if (result.handled) {
            snapshot = ContextSnapshot(
                currentPackage = (action as? Action.Launch)?.packageName
                    ?: AccessibilityServiceHolder.service?.rootInActiveWindow?.packageName?.toString()
                    ?: snapshot.currentPackage,
                lastAction = action,
                lastDirection = (action as? Action.Scroll)?.direction ?: snapshot.lastDirection,
                updatedAtMs = SystemClock.elapsedRealtime(),
            )
            Log.i(TAG, "reflex action=$action package=${snapshot.currentPackage}")
        }
        return result
    }

    private fun isRepeatable(action: Action): Boolean = action is Action.Scroll

    internal fun directionForCommand(command: String): Direction? = when (command) {
        in SCROLL_DOWN -> Direction.DOWN
        in SCROLL_UP -> Direction.UP
        in SCROLL_LEFT -> Direction.LEFT
        in SCROLL_RIGHT -> Direction.RIGHT
        else -> null
    }

    private fun normalize(value: String): String = value
        .lowercase(Locale.ROOT)
        .replace(Regex("[\\p{P}\\p{S}\\s]+"), "")

    private fun splitCommands(value: String): List<String> {
        if (value.isBlank()) return emptyList()
        val withoutPausePrefix = PAUSE_PREFIXES.firstOrNull(value::startsWith)?.let(value::removePrefix) ?: value
        return listOf(withoutPausePrefix).filter(String::isNotBlank)
    }

    private const val TAG = "PhoneReflex"
    private val PAUSE_PREFIXES = listOf("等一下", "等等", "wait", "holdon")
    private val SCROLL_DOWN = setOf("往下滑", "向下滑", "滑下去", "再往下", "再滑一下", "下面", "scrolldown")
    private val SCROLL_UP = setOf("往上滑", "向上滑", "滑上去", "再往上", "上面", "scrollup")
    private val SCROLL_LEFT = setOf("往左滑", "向左滑", "scrollleft")
    private val SCROLL_RIGHT = setOf("往右滑", "向右滑", "scrollright")
    private val BACK = setOf("回去", "返回", "上一頁", "back", "goback")
    private val HOME = setOf("首頁", "回首頁", "home", "gohome")
    private val REPEAT = setOf("再一下", "再一次", "重複", "repeat", "again", "oncemore")
}
