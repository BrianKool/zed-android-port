package com.zdroid

import android.content.Context
import android.provider.Settings
import android.text.TextUtils
import android.util.Base64
import android.util.Log
import io.droidmcp.accessibility.AccessibilityTools
import io.droidmcp.apps.AppsTools
import io.droidmcp.core.DroidMcp
import io.droidmcp.device.DeviceTools
import io.droidmcp.intent.IntentTools
import java.io.File
import java.security.SecureRandom
import java.util.concurrent.Executors

/** Owns the authenticated, process-local MCP server shared by every Zdroid agent. */
object PhoneUseRuntime {
    private const val TAG = "PhoneUseRuntime"
    private const val PORT = 8765
    private const val PREFS = "zdroid_phone_use"
    private const val TOKEN_KEY = "mcp_token"
    private val executor = Executors.newSingleThreadExecutor()

    @Volatile private var server: DroidMcp? = null
    @Volatile private var starting = false
    @Volatile private var desiredRunning = false

    fun initializeIfEnabled(context: Context) {
        if (isAccessibilityEnabled(context)) {
            initialize(context)
        } else {
            writeStatus(context.applicationContext, "Accessibility permission required")
        }
    }

    fun initialize(context: Context) {
        val appContext = context.applicationContext
        desiredRunning = true
        if (server?.isServerRunning() == true || starting) {
            updateStatus(appContext)
            return
        }
        starting = true
        writeStatus(appContext, "Starting Android-native Phone Use...")
        executor.execute {
            try {
                val token = loadOrCreateToken(appContext)
                writePrivateFile(appContext, "token", token)
                val supportedAccessibilityTools = AccessibilityTools.supportedTools(appContext)
                val instance = DroidMcp.builder()
                    .addTools(
                        listOf(
                            PhoneActionPlanTool(appContext),
                            BrowserUseRouterTool(appContext),
                            ObserveSemanticUiTool(),
                            PerformSemanticActionTool(),
                        ),
                    )
                    .addTools(DeviceTools.all(appContext))
                    .addTools(AppsTools.all(appContext))
                    .addTools(IntentTools.all(appContext))
                    .addTools(
                        AccessibilityTools.all(appContext).filter {
                            it.name in supportedAccessibilityTools
                        },
                    )
                    .enableHttpServer(
                        port = PORT,
                        token = token,
                        requireAuth = true,
                        readOnly = false,
                        // The server is app-internal; do not advertise it over mDNS.
                        context = null,
                    )
                    .build()
                instance.startServer()
                if (desiredRunning) {
                    server = instance
                } else {
                    instance.stopServer()
                }
                updateStatus(appContext)
            } catch (error: Throwable) {
                Log.e(TAG, "Unable to start Phone Use MCP", error)
                writeStatus(appContext, "Phone Use service failed: ${error.message ?: error.javaClass.simpleName}")
            } finally {
                starting = false
            }
        }
    }

    fun shutdown(context: Context) {
        val appContext = context.applicationContext
        desiredRunning = false
        executor.execute {
            if (desiredRunning) return@execute
            runCatching { server?.stopServer() }
                .onFailure { Log.w(TAG, "Unable to stop Phone Use MCP cleanly", it) }
            server = null
            writeStatus(appContext, "Accessibility permission required")
        }
    }

    fun updateStatus(context: Context) {
        val status = when {
            server?.isServerRunning() != true -> "Starting Android-native Phone Use..."
            isAccessibilityEnabled(context) -> "Ready - Android Accessibility is connected"
            else -> "Accessibility permission required"
        }
        writeStatus(context, status)
    }

    private fun isAccessibilityEnabled(context: Context): Boolean {
        val component = "${context.packageName}/${ZdroidAccessibilityService::class.java.name}"
        val enabled = Settings.Secure.getString(
            context.contentResolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
        )
        return Settings.Secure.getInt(
            context.contentResolver,
            Settings.Secure.ACCESSIBILITY_ENABLED,
            0,
        ) == 1 && !enabled.isNullOrBlank() && enabled.split(':').any {
            TextUtils.equals(it, component)
        }
    }

    private fun loadOrCreateToken(context: Context): String {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs.getString(TOKEN_KEY, null)?.takeIf { it.length >= 40 }?.let { return it }
        val bytes = ByteArray(32).also(SecureRandom()::nextBytes)
        val token = Base64.encodeToString(bytes, Base64.NO_WRAP or Base64.URL_SAFE or Base64.NO_PADDING)
        check(prefs.edit().putString(TOKEN_KEY, token).commit()) { "could not persist token" }
        return token
    }

    private fun writeStatus(context: Context, value: String) {
        writePrivateFile(context, "status", value)
    }

    private fun writePrivateFile(context: Context, name: String, value: String) {
        val directory = File(context.filesDir, "phone-use")
        check(directory.exists() || directory.mkdirs()) { "could not create Phone Use runtime" }
        File(directory, name).apply {
            writeText(value)
            setReadable(false, false)
            setWritable(false, false)
            setReadable(true, true)
            setWritable(true, true)
        }
    }
}
