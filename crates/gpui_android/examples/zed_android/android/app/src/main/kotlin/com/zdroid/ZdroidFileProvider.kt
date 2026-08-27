package com.zdroid

import androidx.core.content.FileProvider

/**
 * Grants temporary access to a user-selected project file when Zdroid-B asks
 * Android to open it in another app. The provider is not exported; callers
 * only receive access to the single content URI carried by the chooser intent.
 */
class ZdroidFileProvider : FileProvider()
