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
import android.os.PowerManager
import android.net.wifi.WifiManager
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

class AgentForegroundService : Service() {
    private val activeTasks = linkedMapOf<String, String>()
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var keepAliveEnabled = false
    private var wakeLockEnabled = false
    private var batchTaskCount = 0
    private var batchHadFailure = false
    private val batchTaskDescriptions = linkedSetOf<String>()

    override fun onCreate() {
        super.onCreate()
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
                if (!activeTasks.containsKey(taskId)) batchTaskCount += 1
                activeTasks[taskId] = description
                batchTaskDescriptions.add(description)
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
            }
            ACTION_FINISH -> {
                val successful = intent.getBooleanExtra(EXTRA_SUCCESSFUL, false)
                val wasActive = activeTasks.remove(taskId) != null
                if (!wasActive && activeTasks.isEmpty()) {
                    startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
                }
                if (wasActive && !successful) batchHadFailure = true
                if (activeTasks.isEmpty()) {
                    if (wasActive) {
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
                activeTasks.clear()
                stopServiceNow()
                // The integrated editor owns its PTYs and ACP children. Ending
                // the process closes those pipes and terminates this session.
                Process.killProcess(Process.myPid())
            }
        }
        return if (keepAliveEnabled) START_STICKY else START_NOT_STICKY
    }

    override fun onDestroy() {
        releaseSessionLocks()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun reconcileLocks() {
        if (wakeLockEnabled) acquireSessionLocks() else releaseSessionLocks()
    }

    private fun acquireSessionLocks() {
        if (wakeLock?.isHeld == true) return
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "Zdroid:AgentTask",
        ).apply {
            setReferenceCounted(false)
            acquire()
        }
        val wifiManager = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        @Suppress("DEPRECATION")
        wifiLock = wifiManager.createWifiLock(
            WifiManager.WIFI_MODE_FULL_HIGH_PERF,
            "Zdroid:AgentNetwork",
        ).apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseSessionLocks() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        wifiLock?.let { if (it.isHeld) it.release() }
        wifiLock = null
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
            1 -> "1 task running: ${activeTasks.values.first()}"
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
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
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
        private const val CHANNEL_COMPLETION = "zdroid_task_completion"
        private const val RUNNING_NOTIFICATION_ID = 1401
        private const val COMPLETION_NOTIFICATION_ID = 1402
        private const val PREFERENCES = "zdroid_background"
        private const val PREF_ENABLED = "enabled"
        private const val PREF_WAKE_LOCK = "wake_lock"

        fun isBackgroundExecutionEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_ENABLED, true)

        fun persistBackgroundExecutionEnabled(context: Context, enabled: Boolean) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit().putBoolean(PREF_ENABLED, enabled).apply()
        }

        private fun isWakeLockEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PREF_WAKE_LOCK, true)

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
                    "Zdroid-B task completion",
                    NotificationManager.IMPORTANCE_DEFAULT,
                ).apply {
                    description = "Notifies when every active Zdroid-B task has finished"
                    setShowBadge(true)
                },
            )
        }
    }
}
