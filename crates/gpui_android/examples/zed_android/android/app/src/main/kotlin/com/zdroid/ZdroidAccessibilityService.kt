package com.zdroid

import android.content.Intent
import io.droidmcp.accessibility.DroidMcpAccessibilityService

class ZdroidAccessibilityService : DroidMcpAccessibilityService() {
    override fun onServiceConnected() {
        super.onServiceConnected()
        PhoneUseRuntime.initialize(this)
        PhoneUseRuntime.updateStatus(this)
    }

    override fun onUnbind(intent: Intent?): Boolean {
        val result = super.onUnbind(intent)
        PhoneUseRuntime.shutdown(this)
        return result
    }
}
