package com.zdroid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.util.Log

object ZdroidBrowserLauncher {
    private const val TAG = "zed_android_browser"

    fun open(context: Context, url: String) {
        val uri = Uri.parse(url)
        val intent = Intent(Intent.ACTION_VIEW, uri).apply {
            addCategory(Intent.CATEGORY_BROWSABLE)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        val browserPackage = context.packageManager.resolveActivity(
            intent,
            PackageManager.MATCH_DEFAULT_ONLY
        )?.activityInfo?.packageName

        if (!browserPackage.isNullOrBlank()) {
            intent.setPackage(browserPackage)
        }
        Log.i(TAG, "Opening URL with package=$browserPackage")
        context.startActivity(intent)
    }
}

class OpenUrlReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val url = intent.getStringExtra("url")
        if (url.isNullOrBlank()) {
            Log.w("zed_android_browser", "Ignored empty URL request")
            return
        }
        runCatching { ZdroidBrowserLauncher.open(context, url) }
            .onFailure { Log.e("zed_android_browser", "Could not open URL", it) }
    }
}
