package com.zdroid

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.os.PowerManager

/** One process-wide CPU/Wi-Fi lock shared by background tasks and live voice sessions. */
object ZdroidSessionLocks {
    private val owners = linkedSetOf<String>()
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null

    @Synchronized
    fun acquire(context: Context, owner: String) {
        if (!owners.add(owner) && wakeLock?.isHeld == true) return

        val appContext = context.applicationContext
        if (wakeLock?.isHeld != true) {
            val powerManager = appContext.getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = powerManager.newWakeLock(
                PowerManager.PARTIAL_WAKE_LOCK,
                "Zdroid:ActiveSession",
            ).apply {
                setReferenceCounted(false)
                acquire()
            }
        }

        if (wifiLock?.isHeld != true) {
            val wifiManager = appContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            @Suppress("DEPRECATION")
            val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                WifiManager.WIFI_MODE_FULL_LOW_LATENCY
            } else {
                WifiManager.WIFI_MODE_FULL_HIGH_PERF
            }
            wifiLock = wifiManager.createWifiLock(mode, "Zdroid:ActiveNetwork").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
    }

    @Synchronized
    fun release(owner: String) {
        owners.remove(owner)
        if (owners.isNotEmpty()) return

        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        wifiLock?.let { if (it.isHeld) it.release() }
        wifiLock = null
    }
}
