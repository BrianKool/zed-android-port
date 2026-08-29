package com.zdroid

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import java.security.SecureRandom
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap

object MobileActionConfirmation {
    private data class Approval(val operation: String, val expiresAt: Long, @Volatile var approved: Boolean)
    private val approvals = ConcurrentHashMap<String, Approval>()

    fun request(context: Context, operation: String): String {
        prune()
        val token = ByteArray(24).also(SecureRandom()::nextBytes).let {
            Base64.getUrlEncoder().withoutPadding().encodeToString(it)
        }
        approvals[token] = Approval(operation, System.currentTimeMillis() + VALID_MS, false)
        ensureChannel(context)
        val approve = PendingIntent.getBroadcast(
            context,
            token.hashCode(),
            Intent(context, MobileActionConfirmationReceiver::class.java)
                .putExtra(EXTRA_TOKEN, token)
                .putExtra(EXTRA_APPROVED, true),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val deny = PendingIntent.getBroadcast(
            context,
            token.hashCode() xor 0x4d4350,
            Intent(context, MobileActionConfirmationReceiver::class.java)
                .putExtra(EXTRA_TOKEN, token)
                .putExtra(EXTRA_APPROVED, false),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Confirm Mobile Use action")
            .setContentText("Agent requests: $operation")
            .setStyle(NotificationCompat.BigTextStyle().bigText("Agent requests $operation. Review the target app before approving."))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_RECOMMENDATION)
            .setAutoCancel(true)
            .addAction(0, "Deny", deny)
            .addAction(0, "Approve", approve)
            .build()
        (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .notify(token.hashCode(), notification)
        return token
    }

    fun resolve(token: String, approved: Boolean) {
        approvals[token]?.approved = approved
        if (!approved) approvals.remove(token)
    }

    fun consumeApproved(token: String?, operation: String): Boolean {
        if (token == null) return false
        val approval = approvals.remove(token) ?: return false
        return approval.approved && approval.operation == operation && approval.expiresAt >= System.currentTimeMillis()
    }

    private fun prune() {
        val now = System.currentTimeMillis()
        approvals.entries.removeIf { it.value.expiresAt < now }
    }

    private fun ensureChannel(context: Context) {
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "Mobile Use confirmations", NotificationManager.IMPORTANCE_HIGH),
        )
    }

    private const val CHANNEL = "zdroid_mobile_confirmation"
    private const val VALID_MS = 2 * 60 * 1000L
    const val EXTRA_TOKEN = "token"
    const val EXTRA_APPROVED = "approved"
}

class MobileActionConfirmationReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val token = intent.getStringExtra(MobileActionConfirmation.EXTRA_TOKEN) ?: return
        MobileActionConfirmation.resolve(
            token,
            intent.getBooleanExtra(MobileActionConfirmation.EXTRA_APPROVED, false),
        )
        (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .cancel(token.hashCode())
    }
}
