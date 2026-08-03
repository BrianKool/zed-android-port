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
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

class AgentForegroundService : Service() {
    private val activeTasks = linkedMapOf<String, String>()
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onCreate() {
        super.onCreate()
        ensureNotificationChannels(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val taskId = intent?.getStringExtra(EXTRA_TASK_ID) ?: return START_NOT_STICKY
        val description = intent.getStringExtra(EXTRA_DESCRIPTION) ?: "Agent is working"

        when (intent.action) {
            ACTION_START -> {
                activeTasks[taskId] = description
                acquireWakeLock()
                startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
            }
            ACTION_FINISH -> {
                if (activeTasks.isEmpty()) {
                    startForeground(RUNNING_NOTIFICATION_ID, runningNotification())
                }
                activeTasks.remove(taskId)
                val successful = intent.getBooleanExtra(EXTRA_SUCCESSFUL, false)
                postCompletion(taskId, description, successful)
                if (activeTasks.isEmpty()) {
                    releaseWakeLock()
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                    } else {
                        @Suppress("DEPRECATION")
                        stopForeground(true)
                    }
                    stopSelf()
                } else {
                    notificationManager.notify(RUNNING_NOTIFICATION_ID, runningNotification())
                }
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        releaseWakeLock()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun acquireWakeLock() {
        if (wakeLock?.isHeld == true) return
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "Zdroid:AgentTask",
        ).apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    private fun runningNotification(): Notification {
        val description = when (activeTasks.size) {
            0 -> "Agent is working"
            1 -> activeTasks.values.first()
            else -> "${activeTasks.size} agent tasks are running"
        }
        return NotificationCompat.Builder(this, CHANNEL_RUNNING)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Zdroid-B Agent")
            .setContentText(description)
            .setContentIntent(openAppIntent())
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
    }

    private fun postCompletion(taskId: String, description: String, successful: Boolean) {
        val notification = NotificationCompat.Builder(this, CHANNEL_COMPLETION)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle(if (successful) "Agent task completed" else "Agent task stopped")
            .setContentText(description)
            .setContentIntent(openAppIntent())
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()
        notificationManager.notify(COMPLETION_ID_BASE + (taskId.hashCode() and 0x0fffffff), notification)
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
        private const val EXTRA_TASK_ID = "task_id"
        private const val EXTRA_DESCRIPTION = "description"
        private const val EXTRA_SUCCESSFUL = "successful"
        private const val CHANNEL_RUNNING = "zdroid_agent_running"
        private const val CHANNEL_COMPLETION = "zdroid_agent_completion"
        private const val RUNNING_NOTIFICATION_ID = 1401
        private const val COMPLETION_ID_BASE = 2000

        fun startTask(context: Context, taskId: String, description: String) {
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
                    "Agent tasks",
                    NotificationManager.IMPORTANCE_LOW,
                ).apply {
                    description = "Shows while a Zdroid-B agent task is running"
                    setShowBadge(false)
                },
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_COMPLETION,
                    "Agent task results",
                    NotificationManager.IMPORTANCE_DEFAULT,
                ).apply {
                    description = "Notifies when a Zdroid-B agent task finishes"
                },
            )
        }
    }
}
