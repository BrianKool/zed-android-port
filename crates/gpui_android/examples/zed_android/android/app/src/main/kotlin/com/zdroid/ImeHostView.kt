package com.zdroid

import android.content.Context
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection

/// Focusable invisible view that hosts the IME `InputConnection`.
/// GameActivity's `SurfaceView` renders the editor and receives touch
/// input via the NDK native input queue, but it doesn't (and can't
/// easily be subclassed to) provide an `InputConnection` of our
/// choosing. This view sits alongside the `SurfaceView` in the
/// Activity's content hierarchy at 1×1 px (effectively invisible) and
/// is the focus target whenever the editor wants the soft keyboard.
///
/// Focus is requested explicitly from `MainActivity` when Rust signals
/// "this window now has text input focus" (via `set_input_handler`).
/// Touch events stay on the `SurfaceView`; keyboard focus comes here
/// without affecting touch dispatch (the two are independent in
/// Android's input model).
class ImeHostView(context: Context) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    /// Returning `true` also lets Android bind an IME to the view.
    /// That binding is required even when a physical keyboard is
    /// attached: Samsung DeX routes Zhuyin and other CJK composition
    /// through InputConnection rather than plain KeyEvents.
    ///
    /// We gate on:
    ///   1. The user's `android_input.on_screen_keyboard` setting
    ///      (read from the process-wide [SoftKeyboardSetting]
    ///      singleton, populated by Rust's reconcile tick).
    ///   2. Any connected keyboard device with
    ///      `KEYBOARD_TYPE_ALPHABETIC` (BT, USB, hardware dock).
    ///      `Configuration.keyboard` is unreliable — it reports the
    ///      *built-in* physical keyboard and stays at `NOKEYS` for
    ///      tablets even when a BT keyboard is paired and producing
    ///      key events. `InputManager.inputDeviceIds` + each device's
    ///      `keyboardType` is the source of truth.
    override fun onCheckIsTextEditor(): Boolean {
        val userWants = (context as? ImeHost)?.softKeyboardEnabled ?: false
        val hwKeyboardPresent = isPhysicalKeyboardConnected(context)
        return userWants || hwKeyboardPresent
    }

    companion object {
        private fun isPhysicalKeyboardConnected(context: Context): Boolean {
            val im = context.getSystemService(android.hardware.input.InputManager::class.java)
                ?: return false
            for (id in im.inputDeviceIds) {
                val device = im.getInputDevice(id) ?: continue
                if (device.isVirtual) continue
                if (device.keyboardType ==
                    android.view.InputDevice.KEYBOARD_TYPE_ALPHABETIC) {
                    return true
                }
            }
            return false
        }
    }

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection? {
        // Keep the connection alive for a physical keyboard even when
        // the on-screen keyboard is disabled. DeX's Samsung IME needs
        // it to deliver composing text from a hardware Zhuyin layout.
        // Only reject the connection when neither input route is in use.
        val userWants = (context as? ImeHost)?.softKeyboardEnabled ?: false
        val hwKeyboardPresent = isPhysicalKeyboardConnected(context)
        if (!userWants && !hwKeyboardPresent) {
            android.util.Log.i(
                "zdroid_ime",
                "ImeHostView.onCreateInputConnection -> null " +
                    "(userWants=$userWants, hwKeyboard=$hwKeyboardPresent)",
            )
            return null
        }
        val mode = (context as? ImeHost)?.currentImeMode ?: ImeInputMode.CODE_EDITOR
        android.util.Log.i("zdroid_ime", "ImeHostView.onCreateInputConnection mode=$mode")
        // EditorInfo per target kind (see ImeInputMode + ImeTargetKind):
        //
        // - TERMINAL: Termux's TerminalView.java line 280 pattern.
        //   TYPE_TEXT_VARIATION_VISIBLE_PASSWORD disables composition
        //   AND prediction in Gboard / Swiftkey. Means each keystroke
        //   commits directly to the PTY (no setComposingText
        //   accumulation, no autocorrect dump-and-retry).
        //
        // - CODE_EDITOR: NO_SUGGESTIONS kills the autocorrect strip
        //   over code tokens, but we deliberately DON'T set
        //   VISIBLE_PASSWORD — that would also disable CJK
        //   composition, which we want to keep working for users
        //   writing comments / strings in their native script.
        //   IME_MULTI_LINE so Enter inserts a newline instead of
        //   triggering "Done".
        // VISIBLE_PASSWORD remains terminal-only. Applying it to the
        // shared editor/input-field mode disables CJK composition,
        // including Samsung DeX hardware-keyboard Zhuyin input.
        outAttrs.inputType = when (mode) {
            ImeInputMode.TERMINAL ->
                EditorInfo.TYPE_CLASS_TEXT or
                    EditorInfo.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
                    EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            else /* CODE_EDITOR */ ->
                EditorInfo.TYPE_CLASS_TEXT or
                    EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
                    EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
        }
        outAttrs.imeOptions =
            EditorInfo.IME_FLAG_NO_FULLSCREEN or
                EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
        return ZdroidInputConnection(this)
    }

}
