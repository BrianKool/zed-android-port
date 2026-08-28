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
        // The authenticated MCP transport also hosts Office plugins, which do
        // not depend on Accessibility. Keep it alive and only update Phone Use
        // readiness when the accessibility service disconnects.
        PhoneUseRuntime.updateStatus(this)
        return result
    }
}
