package com.zdroid

import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.app.NotificationCompat
import io.droidmcp.core.McpTool
import io.droidmcp.core.ParameterType
import io.droidmcp.core.ToolAnnotations
import io.droidmcp.core.ToolParameter
import io.droidmcp.core.ToolResult
import java.security.SecureRandom
import java.util.Base64
import java.util.concurrent.ConcurrentHashMap

class RequestSecurePasswordTool(private val context: Context) : McpTool {
    override val name = "request_secure_password_input"
    override val description = "Ask the user to securely provide or autofill a password for a semantic password field. The Agent never receives the password."
    override val parameters = listOf(
        ToolParameter("revision", "Semantic UI revision.", ParameterType.INTEGER, true),
        ToolParameter("element_id", "Semantic password-field ID.", ParameterType.STRING, true),
    )
    override val annotations = ToolAnnotations(destructiveHint = true, title = "Secure password assistance")

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        if (!DangerZonePolicy.load(context).allowPasswordAssistance) {
            return ToolResult.error("password_assistance_disabled", "Enable secure password-field assistance in Mobile Use Danger Zone")
        }
        val revision = (params["revision"] as? Number)?.toLong()
            ?: return ToolResult.error("revision is required")
        val elementId = params["element_id"]?.toString().orEmpty()
        if (!SemanticUiRegistry.isSecureTarget(revision, elementId)) {
            return ToolResult.error("not_secure_field", "Target is stale or is not a password field")
        }
        val requestId = SecureCredentialBroker.request(context, revision, elementId)
        return ToolResult.success(
            mapOf(
                "status" to "waiting_for_user",
                "request_id" to requestId,
                "secret_exposed_to_agent" to false,
            ),
        )
    }
}

object SecureCredentialBroker {
    data class Request(val revision: Long, val elementId: String, val expiresAt: Long)
    private val requests = ConcurrentHashMap<String, Request>()

    fun request(context: Context, revision: Long, elementId: String): String {
        val id = ByteArray(24).also(SecureRandom()::nextBytes).let {
            Base64.getUrlEncoder().withoutPadding().encodeToString(it)
        }
        requests[id] = Request(revision, elementId, System.currentTimeMillis() + VALID_MS)
        ensureChannel(context)
        val open = PendingIntent.getActivity(
            context,
            id.hashCode(),
            Intent(context, SecureCredentialActivity::class.java).putExtra(EXTRA_REQUEST_ID, id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Password input required")
            .setContentText("Tap to securely provide or autofill the password")
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_RECOMMENDATION)
            .setAutoCancel(true)
            .setContentIntent(open)
            .build()
        (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .notify(id.hashCode(), notification)
        return id
    }

    fun take(id: String): Request? = requests.remove(id)?.takeIf { it.expiresAt >= System.currentTimeMillis() }

    private fun ensureChannel(context: Context) {
        (context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .createNotificationChannel(
                NotificationChannel(CHANNEL, "Secure credential requests", NotificationManager.IMPORTANCE_HIGH),
            )
    }

    const val EXTRA_REQUEST_ID = "secure_request_id"
    private const val CHANNEL = "zdroid_secure_credentials"
    private const val VALID_MS = 5 * 60 * 1000L
}

class SecureCredentialActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        val requestId = intent.getStringExtra(SecureCredentialBroker.EXTRA_REQUEST_ID)
        val request = requestId?.let(SecureCredentialBroker::take)
        if (request == null) {
            finish()
            return
        }
        val density = resources.displayMetrics.density
        fun dp(value: Int) = (value * density).toInt()
        val input = EditText(this).apply {
            hint = "Password"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            isSingleLine = true
            importantForAutofill = android.view.View.IMPORTANT_FOR_AUTOFILL_YES
            setAutofillHints(android.view.View.AUTOFILL_HINT_PASSWORD)
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(24), dp(28), dp(24), dp(24))
            setBackgroundColor(Color.rgb(30, 31, 34))
            addView(TextView(this@SecureCredentialActivity).apply {
                text = "Secure password input"
                textSize = 20f
                setTextColor(Color.WHITE)
            }, LinearLayout.LayoutParams(-1, -2))
            addView(TextView(this@SecureCredentialActivity).apply {
                text = "The Agent cannot read this value. You may type it or use Android Autofill."
                textSize = 14f
                setTextColor(Color.LTGRAY)
            }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(8) })
            addView(input, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(18) })
            addView(Button(this@SecureCredentialActivity).apply {
                text = "Fill password"
                setOnClickListener {
                    val secret = input.text
                    if (secret.isEmpty()) return@setOnClickListener
                    SemanticUiRegistry.performSecureText(request.revision, request.elementId, secret)
                    secret.clear()
                    finish()
                }
            }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(16) })
        }
        setContentView(root)
        input.requestFocus()
        input.post {
            (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager)
                .showSoftInput(input, InputMethodManager.SHOW_IMPLICIT)
        }
    }
}
