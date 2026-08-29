package com.zdroid

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.os.Process
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

class AgentForegroundService : Service() {
    private val activeTasks = linkedMapOf<String, String>()
    private var keepAliveEnabled = false
    private var wakeLockEnabled = false
    private var batchTaskCount = 0
    private var batchHadFailure = false
    private val batchTaskDescriptions = linkedSetOf<String>()

    override fun onCreate() {
        super.onCreate()
        activeInstance = this
        ensureNotificationChannels(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null) {
            keepAliveEnabled = isBackgroundExecutionEnabled(this)
            if (keepAliveEnabled) {
                wakeLockEnabled = isWakeLockEnabled(this)
                reconcileLocks()
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
                return START_STICKY
            }
            return START_NOT_STICKY
        }
        val taskId = intent.getStringExtra(EXTRA_TASK_ID) ?: "keep-alive"
        val description = intent.getStringExtra(EXTRA_DESCRIPTION) ?: "Agent is working"

        when (intent.action) {
            ACTION_KEEP_ALIVE -> {
                keepAliveEnabled = true
                wakeLockEnabled = isWakeLockEnabled(this)
                reconcileLocks()
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
            }
            ACTION_DISABLE -> {
                keepAliveEnabled = false
                if (activeTasks.isEmpty()) stopServiceNow()
            }
            ACTION_START -> {
                if (activeTasks.isEmpty()) {
                    batchTaskCount = 0
                    batchHadFailure = false
                    batchTaskDescriptions.clear()
                }
                val previousDescription = activeTasks[taskId]
                if (!activeTasks.containsKey(taskId)) {
                    batchTaskCount += 1
                    batchTaskDescriptions.add(description)
                }
                activeTasks[taskId] = description
                reconcileLocks()
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
                if (isWaitingForUser(description)) {
                    if (previousDescription != description && shouldNotifyAttention()) {
                        notificationManager.notify(
                            attentionNotificationId(taskId),
                            attentionNotification(description),
                        )
                    }
                } else {
                    notificationManager.cancel(attentionNotificationId(taskId))
                }
            }
            ACTION_FINISH -> {
                val successful = intent.getBooleanExtra(EXTRA_SUCCESSFUL, false)
                val wasActive = activeTasks.remove(taskId) != null
                reconcileLocks()
                notificationManager.cancel(attentionNotificationId(taskId))
                if (!wasActive && activeTasks.isEmpty()) {
                    startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
                }
                if (wasActive && !successful) batchHadFailure = true
                if (activeTasks.isEmpty()) {
                    if (wasActive && shouldNotifyCompletion()) {
                        notificationManager.notify(
                            COMPLETION_NOTIFICATION_ID,
                            completionNotification(
                                batchTaskCount,
                                batchHadFailure,
                                batchTaskDescriptions.toList(),
                            ),
                        )
                    }
                    if (keepAliveEnabled) {
                        notificationManager.notify(RUNNING_NOTIFICATION_ID, runningNotification())
                    } else {
                        stopServiceNow()
                    }
                } else {
                    notificationManager.notify(RUNNING_NOTIFICATION_ID, runningNotification())
                }
            }
            ACTION_TOGGLE_WAKE_LOCK -> {
                wakeLockEnabled = !wakeLockEnabled
                persistWakeLockEnabled(this, wakeLockEnabled)
                reconcileLocks()
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
            }
            ACTION_EXIT -> {
                persistBackgroundExecutionEnabled(this, false)
                keepAliveEnabled = false
                activeTasks.keys.forEach { notificationManager.cancel(attentionNotificationId(it)) }
                activeTasks.clear()
                stopServiceNow()
                // The integrated editor owns its PTYs and ACP children. Ending
                // the process closes those pipes and terminates this session.
                Process.killProcess(Process.myPid())
            }
        }
        return if (keepAliveEnabled || activeTasks.isNotEmpty()) START_STICKY else START_NOT_STICKY
    }

    override fun onDestroy() {
        if (activeInstance === this) activeInstance = null
        releaseSessionLocks()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun reconcileLocks() {
        if (wakeLockEnabled && activeTasks.isNotEmpty()) acquireSessionLocks()
        else releaseSessionLocks()
    }

    private fun acquireSessionLocks() {
        ZdroidSessionLocks.acquire(this, LOCK_OWNER)
    }

    private fun releaseSessionLocks() {
        ZdroidSessionLocks.release(LOCK_OWNER)
    }

    private fun stopServiceNow() {
        releaseSessionLocks()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
        stopSelf()
    }

    private fun runningNotification(): Notification {
        val description = when (activeTasks.size) {
            0 -> "No tasks running"
            1 -> activeTasks.values.first().let { task ->
                if (isWaitingForUser(task)) task else "1 task running: $task"
            }
            else -> "${activeTasks.size} tasks running"
        }
        return NotificationCompat.Builder(this, CHANNEL_RUNNING)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Zdroid-B - 1 session")
            .setContentText(description)
            .setContentIntent(openAppIntent())
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .addAction(0, "Exit", serviceActionIntent(ACTION_EXIT, 1))
            .addAction(
                0,
                if (wakeLockEnabled) "WakeLock off" else "WakeLock on",
                serviceActionIntent(ACTION_TOGGLE_WAKE_LOCK, 2),
            )
            .build()
    }

    private fun attentionNotification(description: String): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ATTENTION)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Agent needs your input")
            .setContentText(description)
            .setContentIntent(openAppIntent())
            .setCategory(NotificationCompat.CATEGORY_REMINDER)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .build()
    }

    private fun isWaitingForUser(description: String): Boolean =
        description.contains("waiting", ignoreCase = true)

    private fun alertsAllowedWhileVisible(): Boolean =
        !MainActivity.isAppVisible || isNotifyWhileAppVisibleEnabled(this)

    private fun shouldNotifyAttention(): Boolean =
        isAgentAttentionEnabled(this) && alertsAllowedWhileVisible()

    private fun shouldNotifyCompletion(): Boolean =
        isTaskCompletionEnabled(this) && alertsAllowedWhileVisible()

    private fun attentionNotificationId(taskId: String): Int =
        ATTENTION_NOTIFICATION_ID_BASE + (taskId.hashCode() and 0x0fff)

    private fun completionNotification(
        taskCount: Int,
        hadFailure: Boolean,
        descriptions: List<String>,
    ): Notification {
        val title = if (hadFailure) "Task failed" else "Task completed"
        val taskSummary = descriptions.distinct().joinToString(", ")
        val description = when {
            taskCount <= 1 && taskSummary.isNotBlank() && hadFailure -> "$taskSummary finished with an error"
            taskCount <= 1 && taskSummary.isNotBlank() -> "$taskSummary finished successfully"
            hadFailure -> "$taskCount tasks finished; at least one failed"
            else -> "$taskCount tasks finished successfully"
        }
        return NotificationCompat.Builder(this, CHANNEL_COMPLETION)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(title)
            .setContentText(description)
            .setContentIntent(openAppIntent())
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .build()
    }

    private fun serviceActionIntent(action: String, requestCode: Int): PendingIntent {
        val intent = Intent(this, AgentForegroundService::class.java).apply {
            this.action = action
        }
        return PendingIntent.getService(
            this,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun openAppIntent(): PendingIntent {
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        return PendingIntent.getActivity(
            this,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private val notificationManager: NotificationManager
        get() = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    companion object {
        private const val ACTION_START = "com.zdroid.agent.START"
        private const val ACTION_FINISH = "com.zdroid.agent.FINISH"
        private const val ACTION_KEEP_ALIVE = "com.zdroid.agent.KEEP_ALIVE"
        private const val ACTION_DISABLE = "com.zdroid.agent.DISABLE"
        private const val ACTION_TOGGLE_WAKE_LOCK = "com.zdroid.agent.TOGGLE_WAKE_LOCK"
        private const val ACTION_EXIT = "com.zdroid.agent.EXIT"
        private const val EXTRA_TASK_ID = "task_id"
        private const val EXTRA_DESCRIPTION = "description"
        private const val EXTRA_SUCCESSFUL = "successful"
        private const val CHANNEL_RUNNING = "zdroid_agent_running"
        // Channel importance is immutable after first creation. The v2 ID
        // migrates existing installs from the previous non-heads-up channel.
        private const val CHANNEL_COMPLETION = "zdroid_task_completion_v2"
        private const val CHANNEL_ATTENTION = "zdroid_agent_attention"
        private const val RUNNING_NOTIFICATION_ID = 1401
        private const val LEGACY_COMPLETION_NOTIFICATION_ID = 1402
        private const val COMPLETION_NOTIFICATION_ID = 1403
        private const val ATTENTION_NOTIFICATION_ID_BASE = 2000
        private const val PREFERENCES = "zdroid_background"
        private const val PREF_ENABLED = "enabled"
        private const val PREF_WAKE_LOCK = "wake_lock"
        private const val LOCK_OWNER = "agent-background"
        private const val PREF_TASK_COMPLETION = "notify_task_completion"
        private const val PREF_AGENT_ATTENTION = "notify_agent_attention"
        private const val PREF_WHILE_APP_VISIBLE = "notify_while_app_visible"
        @Volatile private var activeInstance: AgentForegroundService? = null

        fun isBackgroundExecutionEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_ENABLED, true)

        fun persistBackgroundExecutionEnabled(context: Context, enabled: Boolean) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit().putBoolean(PREF_ENABLED, enabled).apply()
        }

        private fun isWakeLockEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_WAKE_LOCK, false)

        private fun persistWakeLockEnabled(context: Context, enabled: Boolean) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit().putBoolean(PREF_WAKE_LOCK, enabled).apply()
        }

        fun setBackgroundExecutionEnabled(context: Context, enabled: Boolean) {
            persistBackgroundExecutionEnabled(context, enabled)
            val intent = Intent(context, AgentForegroundService::class.java).apply {
                action = if (enabled) ACTION_KEEP_ALIVE else ACTION_DISABLE
                putExtra(EXTRA_TASK_ID, "keep-alive")
                putExtra(EXTRA_DESCRIPTION, "Background execution")
            }
            if (enabled) {
                ensureNotificationChannels(context)
                ContextCompat.startForegroundService(context, intent)
            } else {
                context.startService(intent)
            }
        }

        fun startTask(context: Context, taskId: String, description: String) {
            if (!isBackgroundExecutionEnabled(context)) return
            ensureNotificationChannels(context)
            val intent = Intent(context, AgentForegroundService::class.java).apply {
                action = ACTION_START
                putExtra(EXTRA_TASK_ID, taskId)
                putExtra(EXTRA_DESCRIPTION, description)
            }
            ContextCompat.startForegroundService(context, intent)
        }

        fun finishTask(
            context: Context,
            taskId: String,
            description: String,
            successful: Boolean,
        ) {
            if (!isBackgroundExecutionEnabled(context)) return
            val intent = Intent(context, AgentForegroundService::class.java).apply {
                action = ACTION_FINISH
                putExtra(EXTRA_TASK_ID, taskId)
                putExtra(EXTRA_DESCRIPTION, description)
                putExtra(EXTRA_SUCCESSFUL, successful)
            }
            ContextCompat.startForegroundService(context, intent)
        }

        fun ensureNotificationChannels(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            manager.cancel(LEGACY_COMPLETION_NOTIFICATION_ID)
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_RUNNING,
                    "Zdroid-B session",
                    NotificationManager.IMPORTANCE_LOW,
                ).apply {
                    description = "Keeps the active Zdroid-B session running in the background"
                    setShowBadge(false)
                },
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_COMPLETION,
                    "Zdroid-B task completion alerts",
                    NotificationManager.IMPORTANCE_HIGH,
                ).apply {
                    description = "Notifies when every active Zdroid-B task has finished"
                    setShowBadge(true)
                },
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ATTENTION,
                    "Zdroid-B agent attention",
                    NotificationManager.IMPORTANCE_HIGH,
                ).apply {
                    description = "Notifies when an Agent is waiting for your answer"
                    setShowBadge(true)
                },
            )
        }

        private fun isTaskCompletionEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_TASK_COMPLETION, true)

        private fun isAgentAttentionEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_AGENT_ATTENTION, true)

        private fun isNotifyWhileAppVisibleEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_WHILE_APP_VISIBLE, false)

        fun setNotificationPreferences(
            context: Context,
            taskCompletion: Boolean,
            agentAttention: Boolean,
            whileAppVisible: Boolean,
        ) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(PREF_TASK_COMPLETION, taskCompletion)
                .putBoolean(PREF_AGENT_ATTENTION, agentAttention)
                .putBoolean(PREF_WHILE_APP_VISIBLE, whileAppVisible)
                .apply()

            val manager = context.getSystemService(NotificationManager::class.java)
            if (!taskCompletion || (!whileAppVisible && MainActivity.isAppVisible)) {
                manager.cancel(COMPLETION_NOTIFICATION_ID)
            }
            if (!agentAttention || (!whileAppVisible && MainActivity.isAppVisible)) {
                activeInstance?.activeTasks?.keys?.forEach { taskId ->
                    manager.cancel(
                        ATTENTION_NOTIFICATION_ID_BASE + (taskId.hashCode() and 0x0fff),
                    )
                }
            }
        }

        fun onAppVisibilityChanged(context: Context, visible: Boolean) {
            if (!visible || isNotifyWhileAppVisibleEnabled(context)) return

            val manager = context.getSystemService(NotificationManager::class.java)
            manager.cancel(COMPLETION_NOTIFICATION_ID)
            activeInstance?.activeTasks?.keys?.forEach { taskId ->
                manager.cancel(
                    ATTENTION_NOTIFICATION_ID_BASE + (taskId.hashCode() and 0x0fff),
                )
            }
        }
    }
}
