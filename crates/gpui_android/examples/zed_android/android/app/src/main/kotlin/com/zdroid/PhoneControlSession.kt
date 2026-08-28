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
import java.util.concurrent.TimeUnit

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

    enum class Outcome { NOT_MATCHED, EXECUTED, PAUSED, UNAVAILABLE, FAILED }

    data class Result(
        val outcome: Outcome,
        val message: String = "",
        val packageName: String? = null,
        val appLabel: String? = null,
        val durationMs: Long = 0L,
    ) {
        val handled: Boolean get() = outcome != Outcome.NOT_MATCHED
        val executed: Boolean get() = outcome == Outcome.EXECUTED
    }

    data class Progress(val packageName: String?, val appLabel: String?)

    private data class LaunchTarget(val packageName: String, val label: String)

    @Volatile private var snapshot = ContextSnapshot(null, null, null, 0L)
    @Volatile private var launchTargets = emptyList<LaunchTarget>()
    @Volatile private var launchTargetsLoadedAtMs = 0L

    fun resolveAndExecute(
        context: Context,
        transcript: String,
        onExecuting: (Progress) -> Unit = {},
    ): Result {
        val startedAt = SystemClock.elapsedRealtime()
        val commands = splitCommands(normalize(transcript))
        if (commands.isEmpty()) return Result(Outcome.NOT_MATCHED)
        val actions = commands.map { command ->
            resolve(context, command) ?: return Result(Outcome.NOT_MATCHED)
        }

        var lastMessage = ""
        var lastPackage: String? = null
        var lastLabel: String? = null
        for (action in actions) {
            val target = executionTarget(context, action)
            onExecuting(Progress(target.first, target.second))
            val result = execute(context, action)
            if (!result.executed) return result.copy(durationMs = SystemClock.elapsedRealtime() - startedAt)
            lastMessage = result.message
            lastPackage = result.packageName
            lastLabel = result.appLabel
        }
        return Result(
            Outcome.EXECUTED,
            lastMessage,
            lastPackage,
            lastLabel,
            SystemClock.elapsedRealtime() - startedAt,
        )
    }

    fun contextSnapshot(): ContextSnapshot = snapshot

    private fun resolve(context: Context, command: String): Action? {
        for (candidate in commandCandidates(command)) {
            directionForCanonicalCommand(candidate)?.let { return Action.Scroll(it) }
            when {
                candidate in BACK -> return Action.Global(AccessibilityService.GLOBAL_ACTION_BACK, "Back")
                candidate in HOME -> return Action.Global(AccessibilityService.GLOBAL_ACTION_HOME, "Home")
                candidate in REPEAT -> return snapshot.lastAction?.takeIf(::isRepeatable)
                candidate.startsWith("打開") -> return resolveLaunch(context, candidate.removePrefix("打開"))
                candidate.startsWith("開啟") -> return resolveLaunch(context, candidate.removePrefix("開啟"))
                candidate.startsWith("open") -> return resolveLaunch(context, candidate.removePrefix("open"))
            }
        }
        return null
    }

    private fun resolveLaunch(context: Context, requestedName: String): Action.Launch? {
        if (requestedName.isBlank()) return null
        val normalizedName = normalize(requestedName)
        KNOWN_APPS[normalizedName]?.let { packageName ->
            context.packageManager.getLaunchIntentForPackage(packageName)?.let {
                val label = context.packageManager.getApplicationLabel(
                    context.packageManager.getApplicationInfo(packageName, 0),
                ).toString()
                return Action.Launch(packageName, label)
            }
        }
        val matches = cachedLaunchTargets(context).filter { target ->
            val normalizedLabel = normalize(target.label)
            normalizedLabel == normalizedName || normalizedLabel.contains(normalizedName)
        }
        val match = matches.singleOrNull() ?: return null
        return Action.Launch(match.packageName, match.label)
    }

    private fun executionTarget(context: Context, action: Action): Pair<String?, String?> = when (action) {
        is Action.Launch -> action.packageName to action.label
        else -> {
            val packageName = AccessibilityServiceHolder.service?.rootInActiveWindow?.packageName?.toString()
            packageName to packageName?.let { appLabel(context, it) }
        }
    }

    private fun execute(
        context: Context,
        action: Action,
    ): Result {
        val actionStartedAt = SystemClock.elapsedRealtime()
        val service = AccessibilityServiceHolder.service
        val result = when (action) {
            is Action.Scroll -> {
                if (service == null) {
                    return Result(Outcome.UNAVAILABLE, "Enable Phone Use in Accessibility settings")
                }
                val activePackage = service.rootInActiveWindow?.packageName?.toString()
                    ?: return Result(Outcome.UNAVAILABLE, "No active Android window is available")
                if (activePackage == context.packageName) {
                    return Result(
                        Outcome.PAUSED,
                        "Return to the app you want to control, then repeat the command",
                        activePackage,
                        appLabel(context, activePackage),
                    )
                }
                val metrics = context.resources.displayMetrics
                val points = actionPlanScrollPoints(
                    action.direction.name.lowercase(Locale.ROOT),
                    metrics.widthPixels.toFloat(),
                    metrics.heightPixels.toFloat(),
                ) ?: return Result(Outcome.FAILED, "Unsupported scroll direction")
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
                Result(
                    if (dispatched) Outcome.EXECUTED else Outcome.FAILED,
                    if (dispatched) "Scrolled ${action.direction.name.lowercase()}" else "Android rejected the scroll gesture",
                    activePackage,
                    appLabel(context, activePackage),
                )
            }
            is Action.Global -> {
                if (service == null) {
                    return Result(Outcome.UNAVAILABLE, "Enable Phone Use in Accessibility settings")
                }
                val activePackage = service.rootInActiveWindow?.packageName?.toString()
                if (activePackage == context.packageName) {
                    return Result(
                        Outcome.PAUSED,
                        "Return to the app you want to control, then repeat the command",
                        activePackage,
                        appLabel(context, activePackage),
                    )
                }
                val performed = service.performGlobalAction(action.action)
                Result(
                    if (performed) Outcome.EXECUTED else Outcome.FAILED,
                    if (performed) action.label else "Android rejected ${action.label}",
                    activePackage,
                    activePackage?.let { appLabel(context, it) },
                )
            }
            is Action.Launch -> {
                val launchIntent = context.packageManager.getLaunchIntentForPackage(action.packageName)
                    ?: return Result(Outcome.FAILED, "${action.label} is not available")
                context.startActivity(launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                Result(Outcome.EXECUTED, "Opened ${action.label}", action.packageName, action.label)
            }
        }
        if (result.executed) {
            snapshot = ContextSnapshot(
                currentPackage = (action as? Action.Launch)?.packageName
                    ?: AccessibilityServiceHolder.service?.rootInActiveWindow?.packageName?.toString()
                    ?: snapshot.currentPackage,
                lastAction = action,
                lastDirection = (action as? Action.Scroll)?.direction ?: snapshot.lastDirection,
                updatedAtMs = SystemClock.elapsedRealtime(),
            )
            Log.i(
                TAG,
                "reflex action=$action package=${snapshot.currentPackage} " +
                    "duration_ms=${SystemClock.elapsedRealtime() - actionStartedAt}",
            )
        }
        return result
    }

    private fun isRepeatable(action: Action): Boolean = action is Action.Scroll

    internal fun directionForCommand(command: String): Direction? =
        commandCandidates(normalize(command)).firstNotNullOfOrNull(::directionForCanonicalCommand)

    private fun directionForCanonicalCommand(command: String): Direction? = when (command) {
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
        return withoutPausePrefix.split(COMMAND_SEPARATOR).filter(String::isNotBlank)
    }

    private fun commandCandidates(value: String): List<String> {
        val candidates = linkedSetOf(value)
        var current = value
        while (true) {
            val prefix = POLITE_PREFIXES.firstOrNull(current::startsWith) ?: break
            current = current.removePrefix(prefix)
            if (current.isNotBlank()) candidates += current
        }
        val withSuffixes = candidates.toList()
        for (candidate in withSuffixes) {
            var withoutSuffix = candidate
            while (true) {
                val suffix = POLITE_SUFFIXES.firstOrNull(withoutSuffix::endsWith) ?: break
                withoutSuffix = withoutSuffix.removeSuffix(suffix)
                if (withoutSuffix.isNotBlank()) candidates += withoutSuffix
            }
        }
        return candidates.toList()
    }

    @Synchronized
    private fun cachedLaunchTargets(context: Context): List<LaunchTarget> {
        val now = SystemClock.elapsedRealtime()
        if (launchTargets.isNotEmpty() && now - launchTargetsLoadedAtMs < LAUNCH_CACHE_TTL_MS) {
            return launchTargets
        }
        val packageManager = context.packageManager
        launchTargets = packageManager.getInstalledApplications(0).mapNotNull { app ->
            packageManager.getLaunchIntentForPackage(app.packageName) ?: return@mapNotNull null
            LaunchTarget(app.packageName, packageManager.getApplicationLabel(app).toString())
        }
        launchTargetsLoadedAtMs = now
        return launchTargets
    }

    private fun appLabel(context: Context, packageName: String): String = runCatching {
        val info = context.packageManager.getApplicationInfo(packageName, 0)
        context.packageManager.getApplicationLabel(info).toString()
    }.getOrDefault(packageName)

    private const val TAG = "PhoneReflex"
    private val PAUSE_PREFIXES = listOf("等一下", "等等", "wait", "holdon")
    private val POLITE_PREFIXES = listOf("可以請你幫我", "可以幫我", "麻煩幫我", "請幫我", "幫我", "麻煩", "請你", "請")
    private val POLITE_SUFFIXES = listOf("可以嗎", "好嗎", "一下", "謝謝", "吧", "嗎")
    private val COMMAND_SEPARATOR = Regex("(?:然後|接著|之後|andthen)")
    private val SCROLL_DOWN = setOf("往下滑", "向下滑", "滑下去", "再往下", "再滑一下", "下面", "scrolldown")
    private val SCROLL_UP = setOf("往上滑", "向上滑", "滑上去", "再往上", "上面", "scrollup")
    private val SCROLL_LEFT = setOf("往左滑", "向左滑", "scrollleft")
    private val SCROLL_RIGHT = setOf("往右滑", "向右滑", "scrollright")
    private val BACK = setOf("回去", "返回", "上一頁", "back", "goback")
    private val HOME = setOf("首頁", "回首頁", "home", "gohome")
    private val REPEAT = setOf("再一下", "再一次", "重複", "repeat", "again", "oncemore")
    private val KNOWN_APPS = mapOf(
        "youtube" to "com.google.android.youtube",
        "chrome" to "com.android.chrome",
        "googlechrome" to "com.android.chrome",
    )
    private val LAUNCH_CACHE_TTL_MS = TimeUnit.MINUTES.toMillis(5)
}
