package com.zdroid

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.view.Gravity
import android.view.KeyEvent
import android.view.MotionEvent
import android.widget.LinearLayout
import android.widget.TextView

/// Termux-style two-row key grid that floats above the soft keyboard.
///
/// **Sticky modifier semantics (Termux convention).** Tapping `Ctrl`
/// or `Alt` once arms the modifier for the next regular key; the
/// next key fires as `Ctrl+key` (or `Alt+key`) and the modifier
/// auto-clears. Tapping the same modifier twice in a row latches
/// it on, so every subsequent regular key keeps the modifier until
/// the user taps it a third time to unlatch. The button fill
/// changes color across the three states (off → armed → latched)
/// so the user can see what's about to happen before they touch a
/// regular key.
///
/// **Routing.** Every regular-key press synthesizes a `KeyEvent`
/// pair (ACTION_DOWN + ACTION_UP) and hands them to the hosting
/// Activity's `dispatchKeyEvent`. On `MainActivity` that lands on
/// the GameActivity input pipeline; on `ExtraWindowActivity` it
/// lands on the `nativeOnExtraKeyEvent` JNI bridge. Either way the
/// editor sees a normal hardware-style `KeyDown`/`KeyUp` with the
/// appropriate `metaState` bits set.
class ExtraKeysView(
    context: Context,
    /// Invoked on every modifier state change so the hosting
    /// Activity can publish `(pending, locked)` to the
    /// `ZdroidInputConnection.commitText` intercept path. Called
    /// on the UI thread inside `refreshModifierVisuals`. Pass
    /// `null` for hosts that don't need the intercept (e.g. test
    /// scaffolds).
    private val onModifierStateChanged: ((pending: Int, locked: Int) -> Unit)? = null,
) : LinearLayout(context) {

    private var pendingMeta: Int = 0
    private var lockedMeta: Int = 0

    /// Each modifier button keeps a reference to its own
    /// [GradientDrawable] fill so [refreshModifierVisuals] can
    /// mutate the color without rebuilding the whole [RippleDrawable]
    /// (which would lose the in-flight ripple animation).
    private data class ModifierEntry(val button: TextView, val fill: GradientDrawable)
    private val modifierEntries = mutableMapOf<Int, ModifierEntry>()

    private val density = resources.displayMetrics.density

    init {
        orientation = VERTICAL
        setBackgroundColor(Color.parseColor("#E61E1E2E"))
        val pad = dp(2)
        setPadding(pad, pad, pad, pad)

        val firstRow = makeRow()
        addRegularKey(firstRow, "ESC", KeyEvent.KEYCODE_ESCAPE)
        addRegularKey(firstRow, "/", KeyEvent.KEYCODE_SLASH)
        addRegularKey(firstRow, "-", KeyEvent.KEYCODE_MINUS)
        addRegularKey(firstRow, "HOME", KeyEvent.KEYCODE_MOVE_HOME)
        addRegularKey(firstRow, "↑", KeyEvent.KEYCODE_DPAD_UP)
        addRegularKey(firstRow, "END", KeyEvent.KEYCODE_MOVE_END)
        addRegularKey(firstRow, "PGUP", KeyEvent.KEYCODE_PAGE_UP)
        addView(firstRow)

        val secondRow = makeRow()
        addRegularKey(secondRow, "TAB", KeyEvent.KEYCODE_TAB)
        addModifierKey(secondRow, "CTRL", KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON)
        addModifierKey(secondRow, "ALT", KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON)
        addRegularKey(secondRow, "←", KeyEvent.KEYCODE_DPAD_LEFT)
        addRegularKey(secondRow, "↓", KeyEvent.KEYCODE_DPAD_DOWN)
        addRegularKey(secondRow, "→", KeyEvent.KEYCODE_DPAD_RIGHT)
        addRegularKey(secondRow, "PGDN", KeyEvent.KEYCODE_PAGE_DOWN)
        addView(secondRow)
    }

    private fun makeRow() = LinearLayout(context).apply {
        orientation = HORIZONTAL
        layoutParams = LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT)
    }

    private fun addRegularKey(row: LinearLayout, label: String, keyCode: Int) {
        val button = makeButton(label, isModifier = false)
        attachRepeatingTouchHandler(button, keyCode)
        row.addView(button)
    }

    /// Press-and-hold auto-repeat for regular keys (arrows, Esc,
    /// Tab). Matches desktop keyboard behavior: tap fires once,
    /// hold fires once at touch-down then repeats at the system's
    /// `keyRepeatTimeout` (initial delay) / `keyRepeatDelay`
    /// (interval) cadence until the user lifts.
    ///
    /// Modifier keys (`Ctrl`, `Alt`, `Shift`) deliberately don't get
    /// this handler because they're state toggles, not key fires.
    /// Holding Ctrl shouldn't spam `Ctrl` events.
    ///
    /// `isPressed = true` on DOWN triggers the `RippleDrawable`
    /// animation, so the visual feedback we'd otherwise lose by
    /// replacing `setOnClickListener` with `setOnTouchListener`
    /// still fires.
    private fun attachRepeatingTouchHandler(button: android.view.View, keyCode: Int) {
        val initialDelay = android.view.ViewConfiguration.getKeyRepeatTimeout().toLong()
        val repeatInterval = android.view.ViewConfiguration.getKeyRepeatDelay().toLong()
        val repeatRunnable = object : Runnable {
            override fun run() {
                // During auto-repeat, only the *locked* portion of
                // the modifier carries forward — pending was
                // consumed by the first fire on DOWN. So Ctrl-tap +
                // hold-Right-arrow walks word-by-word for one move,
                // then plain Right-arrow chars after that. Latched
                // Ctrl (double-tap) keeps Ctrl on every repeat.
                fireKey(keyCode, lockedMeta)
                button.postDelayed(this, repeatInterval)
            }
        }
        button.setOnTouchListener { v, ev ->
            when (ev.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    v.isPressed = true
                    val meta = pendingMeta or lockedMeta
                    fireKey(keyCode, meta)
                    if (pendingMeta != 0) {
                        pendingMeta = 0
                        refreshModifierVisuals()
                    }
                    v.postDelayed(repeatRunnable, initialDelay)
                    true
                }
                MotionEvent.ACTION_UP,
                MotionEvent.ACTION_CANCEL -> {
                    v.isPressed = false
                    v.removeCallbacks(repeatRunnable)
                    true
                }
                else -> false
            }
        }
    }

    private fun addModifierKey(row: LinearLayout, label: String, metaBits: Int) {
        val (button, fill) = makeButtonWithFill(label, isModifier = true)
        modifierEntries[metaBits] = ModifierEntry(button, fill)
        button.setOnTouchListener { view, event ->
            if (event.actionMasked != MotionEvent.ACTION_DOWN) {
                view.isPressed = false
                return@setOnTouchListener true
            }
            view.isPressed = true
            // Three-state cycle: off → armed (pending) → latched
            // (locked) → off. Tapping any regular key in between
            // clears `pending` but leaves `locked` alone, so the
            // user can latch Ctrl, hit a sequence of keys, then
            // un-latch with another Ctrl tap.
            when {
                lockedMeta and metaBits != 0 -> lockedMeta = lockedMeta and metaBits.inv()
                pendingMeta and metaBits != 0 -> {
                    pendingMeta = pendingMeta and metaBits.inv()
                    lockedMeta = lockedMeta or metaBits
                }
                else -> pendingMeta = pendingMeta or metaBits
            }
            refreshModifierVisuals()
            true
        }
        row.addView(button)
    }

    private fun makeButton(label: String, isModifier: Boolean): TextView {
        return makeButtonWithFill(label, isModifier).first
    }

    private fun makeButtonWithFill(
        label: String,
        isModifier: Boolean,
    ): Pair<TextView, GradientDrawable> {
        val cornerRadius = dp(4).toFloat()

        // Inner rounded fill — what the user actually "sees" as
        // the button surface. Color is mutated in
        // refreshModifierVisuals for sticky-modifier states.
        val fill = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            this.cornerRadius = cornerRadius
            setColor(COLOR_OFF)
        }
        // Mask: same shape, opaque white. RippleDrawable uses this
        // to clip the ripple animation to the rounded rect, so
        // the ripple doesn't bleed past the corners on tap.
        val mask = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            this.cornerRadius = cornerRadius
            setColor(Color.WHITE)
        }
        val ripple = RippleDrawable(
            ColorStateList.valueOf(Color.parseColor("#9989B4FA")),
            fill,
            mask,
        )

        val button = TextView(context).apply {
            text = label
            textSize = when {
                label.length >= 4 -> 10f
                isModifier -> 11f
                else -> 12f
            }
            typeface = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
            setTextColor(Color.parseColor("#CDD6F4")) // Catppuccin Mocha "Text"
            gravity = Gravity.CENTER
            // Belt-and-suspenders: even with `fillViewport=true`
            // upstream, force the label to a single line so a
            // future style/padding tweak that re-tightens the
            // width can't bring back the per-character vertical
            // wrap seen on the first iteration of this view.
            maxLines = 1
            setSingleLine(true)
            isHorizontalFadingEdgeEnabled = false
            background = ripple
            isClickable = true
            isFocusable = false
            isFocusableInTouchMode = false

            val padH = dp(2)
            val padV = dp(4)
            setPadding(padH, padV, padH, padV)
            minHeight = dp(32)

            minWidth = 0
            val margin = dp(1)
            layoutParams = LinearLayout.LayoutParams(
                /* width = */ 0,
                /* height = */ LinearLayout.LayoutParams.WRAP_CONTENT,
                /* weight = */ 1f,
            ).apply {
                setMargins(margin, margin, margin, margin)
            }
        }
        return button to fill
    }

    private fun refreshModifierVisuals() {
        for ((bits, entry) in modifierEntries) {
            val color = when {
                lockedMeta and bits != 0 -> COLOR_LATCHED
                pendingMeta and bits != 0 -> COLOR_ARMED
                else -> COLOR_OFF
            }
            entry.fill.setColor(color)
        }
        onModifierStateChanged?.invoke(pendingMeta, lockedMeta)
    }

    /// Called from the hosting Activity after
    /// `ZdroidInputConnection.commitText` consumes a single
    /// character and re-fires it as a `KeyEvent` with our
    /// modifier metaState. Drops the *pending* (one-shot) portion
    /// of the state and refreshes button visuals; the locked
    /// portion stays in effect for subsequent keys.
    fun consumePendingModifier() {
        if (pendingMeta == 0) return
        pendingMeta = 0
        refreshModifierVisuals()
    }

    fun clearModifiers() {
        if (pendingMeta == 0 && lockedMeta == 0) return
        pendingMeta = 0
        lockedMeta = 0
        refreshModifierVisuals()
    }

    private fun fireKey(keyCode: Int, metaState: Int) {
        // Route through the same JNI bridge the soft keyboard's
        // hardware-key fallback uses (`ZdroidInputConnection.sendKeyEvent`).
        // Calling `activity.dispatchKeyEvent` here doesn't work on
        // `MainActivity` because GameActivity reads key input from the
        // NDK input queue, not from the Java view tree's dispatch
        // path; synthetic events delivered via `dispatchKeyEvent`
        // never reach the gpui-side editor and end up consumed by
        // `BaseInputConnection`'s fallbacks (which is why Esc / Tab /
        // arrows looked partly working but `Ctrl` did nothing — there
        // was nothing for the modifier to attach to). The IME JNI
        // path lands as a real `PlatformInput::KeyDown` with the
        // `metaState` we set, so Ctrl+arrow / Alt+arrow combos work
        // out of the box, and modifiers carried into a regular key
        // tap on this row are preserved end-to-end.
        val windowId = (context as? ImeHost)?.imeWindowId ?: 0L
        NativeBridge.nativeImeSendKeyEvent(
            windowId,
            KeyEvent.ACTION_DOWN,
            keyCode,
            metaState,
            /* repeatCount = */ 0,
        )
        NativeBridge.nativeImeSendKeyEvent(
            windowId,
            KeyEvent.ACTION_UP,
            keyCode,
            metaState,
            0,
        )
    }

    private fun dp(value: Int): Int = (value * density).toInt()

    companion object {
        // Catppuccin Mocha surface palette for the three modifier
        // states. Surface0 (off) sits well against the row's
        // base background; Surface1 (armed) is one shade brighter
        // so an armed modifier reads as "lifted"; Blue (latched)
        // is the accent color, unambiguously different from any
        // regular key fill.
        private val COLOR_OFF = Color.parseColor("#313244")
        private val COLOR_ARMED = Color.parseColor("#89B4FA")
        private val COLOR_LATCHED = Color.parseColor("#CBA6F7")
    }
}
