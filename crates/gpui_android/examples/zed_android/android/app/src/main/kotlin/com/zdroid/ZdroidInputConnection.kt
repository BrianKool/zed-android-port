package com.zdroid

import android.util.Log
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.ExtractedText
import android.view.inputmethod.ExtractedTextRequest
import android.view.inputmethod.InputMethodManager
import java.util.concurrent.atomic.AtomicLong

private const val TAG = "zdroid_ime"

/// Bridge from Android's IME (Gboard, Swiftkey, etc.) into gpui's
/// `PlatformInputHandler`. The IME calls methods here for every text
/// edit it wants to perform; we forward each into Rust via
/// `NativeBridge`, and Rust dispatches into the focused window's
/// `PlatformInputHandler` (the same trait macOS NSTextInputClient
/// and Linux text-input-v3 wire to).
///
/// Composition (CJK / prediction / gesture typing) uses
/// `setComposingText` / `finishComposingText`. Plain commits (final
/// keystrokes, voice input) use `commitText`. Backspace / delete uses
/// `deleteSurroundingText`. Hardware-style key events that the IME
/// wants to deliver (Enter, arrows) flow through `sendKeyEvent` and
/// land on the existing `events/keyboard.rs` translator.
///
/// Returns `true` from each method to signal the IME we consumed the
/// event. Returning `false` would cause the IME to fall back to
/// posting the text as KeyEvents, which we explicitly don't want
/// (loses composition info).
class ZdroidInputConnection(private val hostView: View) :
    BaseInputConnection(hostView, /* fullEditor = */ true) {
    private val connectionId = nextConnectionId.getAndIncrement()
    private val windowId: Long = (hostView.context as? ImeHost)?.imeWindowId ?: 0L
    private var shadowState: ImeTextState =
        (hostView.context as? ImeHost)?.getImeTextState() ?: ImeTextState.EMPTY
    private var localRevision: Long = shadowState.revision
    private var batchDepth = 0
    private var deferredState: ImeTextState? = null

    init {
        NativeBridge.nativeImeConnectionOpened(windowId, connectionId, localRevision)
    }

    private fun nextRevision(): Long = ++localRevision

    fun reconcileTextState(state: ImeTextState): Boolean {
        if (state.revision < localRevision) return false
        if (batchDepth > 0) {
            if (deferredState == null || state.revision >= deferredState!!.revision) {
                deferredState = state
            }
            return false
        }
        shadowState = state
        localRevision = state.revision
        return true
    }

    override fun beginBatchEdit(): Boolean {
        batchDepth += 1
        return true
    }

    override fun endBatchEdit(): Boolean {
        if (batchDepth == 0) return false
        batchDepth -= 1
        if (batchDepth == 0) {
            deferredState?.let { state ->
                deferredState = null
                if (reconcileTextState(state)) {
                    val imm = hostView.context.getSystemService(InputMethodManager::class.java)
                    imm?.updateSelection(
                        hostView,
                        state.selectionStart,
                        state.selectionEnd,
                        state.composingStart,
                        state.composingEnd,
                    )
                }
            }
        }
        return true
    }

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val s = text?.toString() ?: ""
        // Modifier intercept: when the user has armed (or latched)
        // a sticky modifier on `ExtraKeysView` and the next event
        // is a single-character soft-keyboard commit, re-synthesize
        // the commit as a `KeyEvent` carrying the modifier in its
        // metaState. Without this, Gboard's `commitText("c")` lands
        // at the editor as a literal 'c' insert, because Gboard's
        // commit path has no slot for `META_CTRL_*` bits.
        //
        // The lookup uses Android's [KeyCharacterMap.VIRTUAL_KEYBOARD]
        // which maps printable chars to their keyCode + the meta
        // bits needed to type them (e.g. 'C' returns
        // KEYCODE_C / META_SHIFT_ON). We OR our modifier on top so
        // Ctrl+Shift+C goes through correctly when both are active.
        // Multi-character commits skip the intercept and fall
        // through to the plain text path; CJK composition + paste
        // shouldn't be re-keyed.
        val host = hostView.context as? ImeHost
        val modifier = host?.extraKeysModifierState ?: 0
        if (modifier != 0 && s.length == 1) {
            val keyMap = android.view.KeyCharacterMap
                .load(android.view.KeyCharacterMap.VIRTUAL_KEYBOARD)
            val events = keyMap.getEvents(charArrayOf(s[0]))
            val firstDown = events?.firstOrNull { it.action == KeyEvent.ACTION_DOWN }
            if (firstDown != null && firstDown.keyCode != KeyEvent.KEYCODE_UNKNOWN) {
                val combinedMeta = modifier or firstDown.metaState
                Log.i(
                    TAG,
                    "IC.commitText w=$windowId intercepted len=${s.length} as " +
                        "key=${firstDown.keyCode} meta=0x${Integer.toHexString(combinedMeta)}"
                )
                NativeBridge.nativeImeSendKeyEvent(
                    windowId,
                    KeyEvent.ACTION_DOWN,
                    firstDown.keyCode,
                    combinedMeta,
                    0,
                )
                NativeBridge.nativeImeSendKeyEvent(
                    windowId,
                    KeyEvent.ACTION_UP,
                    firstDown.keyCode,
                    combinedMeta,
                    0,
                )
                host?.clearExtrasPendingModifier()
                return true
            }
        }
        // Vim command-mode routing: when the focused editor is in a
        // vim command mode the committed text has to reach the editor
        // as key *events* so vim's keymap reads it as motions /
        // operators (`j`, `d`, `w`) instead of inserting the literal
        // characters. We re-key the whole commit through
        // `KeyCharacterMap` (same mechanism as the modifier intercept
        // above) so the full down/up sequence carries the right keyCode
        // and metaState — shifted letters (`G`, `A`) and symbols (`$`,
        // `:`, `/`) come through as the keystrokes vim expects. If any
        // char has no virtual-keyboard mapping `getEvents` returns null
        // for the batch; we fall through to a plain commit so nothing is
        // silently dropped (e.g. an emoji pasted in normal mode).
        if (s.isNotEmpty() && NativeBridge.nativeImeRouteAsKeys()) {
            val keyMap = android.view.KeyCharacterMap
                .load(android.view.KeyCharacterMap.VIRTUAL_KEYBOARD)
            val events = keyMap.getEvents(s.toCharArray())
            if (events != null && events.isNotEmpty()) {
                Log.i(TAG, "IC.commitText w=$windowId vim-route len=${s.length} as ${events.size} key events")
                for (ev in events) {
                    NativeBridge.nativeImeSendKeyEvent(
                        windowId,
                        ev.action,
                        ev.keyCode,
                        ev.metaState,
                        ev.repeatCount,
                    )
                }
                return true
            }
            Log.i(TAG, "IC.commitText w=$windowId vim-route fallthrough (no keymap) len=${s.length}")
        }
        Log.d(TAG, "IC.commitText w=$windowId length=${s.length} cursor=$newCursorPosition")
        val revision = nextRevision()
        shadowState = shadowState.withCommittedText(s, newCursorPosition, revision)
        NativeBridge.nativeImeCommitText(windowId, connectionId, s, newCursorPosition, revision)
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val s = text?.toString() ?: ""
        Log.d(TAG, "IC.setComposingText w=$windowId length=${s.length} cursor=$newCursorPosition")
        val revision = nextRevision()
        shadowState = shadowState.withComposingText(s, newCursorPosition, revision)
        NativeBridge.nativeImeSetComposingText(windowId, connectionId, s, newCursorPosition, revision)
        return true
    }

    override fun setComposingRegion(start: Int, end: Int): Boolean {
        val revision = nextRevision()
        shadowState = shadowState.withComposingRegion(start, end, revision)
        NativeBridge.nativeImeSetComposingRegion(windowId, connectionId, start, end, revision)
        return true
    }

    override fun setSelection(start: Int, end: Int): Boolean {
        val revision = nextRevision()
        shadowState = shadowState.withSelection(start, end, revision)
        NativeBridge.nativeImeSetSelection(windowId, connectionId, start, end, revision)
        return true
    }

    override fun finishComposingText(): Boolean {
        Log.i(TAG, "IC.finishComposingText w=$windowId")
        val revision = nextRevision()
        shadowState = shadowState.withoutComposition(revision)
        NativeBridge.nativeImeFinishComposingText(windowId, connectionId, revision)
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        Log.i(TAG, "IC.deleteSurroundingText w=$windowId before=$beforeLength after=$afterLength")
        val revision = nextRevision()
        shadowState = shadowState.deletingSurroundingText(beforeLength, afterLength, revision)
        NativeBridge.nativeImeDeleteSurroundingText(
            windowId,
            connectionId,
            beforeLength,
            afterLength,
            revision,
        )
        return true
    }

    override fun sendKeyEvent(event: KeyEvent?): Boolean {
        event ?: return false
        // Fold in any armed ExtraKeysView modifier (Ctrl / Alt / Shift)
        // on keys the IME delivers through this path. The important one
        // is Enter (keyCode 66), which Samsung's keyboard routes here, so
        // an armed Shift yields Shift+Enter (e.g. a newline in Claude
        // Code's prompt), Ctrl yields Ctrl+Enter, and so on. The
        // commitText intercept only covers single-character commits, not
        // these hardware-style keys, so Enter / Backspace / arrows
        // arriving here would otherwise drop the modifier. The OR is
        // idempotent if the keyboard already set the same bit. Pending
        // (one-shot) modifiers clear on the key's UP so the modifier
        // spans the whole down/up pair; a latched modifier stays until
        // the user unlatches it on the row.
        val host = hostView.context as? ImeHost
        val modifier = host?.extraKeysModifierState ?: 0
        val meta = event.metaState or modifier
        Log.i(
            TAG,
            "IC.sendKeyEvent w=$windowId action=${event.action} keyCode=${event.keyCode} " +
                "meta=0x${Integer.toHexString(meta)} (extras=0x${Integer.toHexString(modifier)}) " +
                "repeat=${event.repeatCount} unicode=${event.unicodeChar} charsLength=${event.characters?.length ?: 0}"
        )
        NativeBridge.nativeImeSendKeyEvent(
            windowId,
            event.action,
            event.keyCode,
            meta,
            event.repeatCount,
        )
        if (modifier != 0 && event.action == KeyEvent.ACTION_UP) {
            host?.clearExtrasPendingModifier()
        }
        return true
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        Log.i(TAG, "IC.performEditorAction w=$windowId action=$actionCode")
        NativeBridge.nativeImePerformEditorAction(windowId, actionCode)
        return true
    }

    override fun performContextMenuAction(id: Int): Boolean {
        val command = when (id) {
            android.R.id.cut -> COMMAND_CUT
            android.R.id.copy -> COMMAND_COPY
            android.R.id.paste, android.R.id.pasteAsPlainText -> COMMAND_PASTE
            android.R.id.selectAll -> COMMAND_SELECT_ALL
            else -> return super.performContextMenuAction(id)
        }
        Log.i(TAG, "IC.performContextMenuAction w=$windowId id=$id command=$command")
        NativeBridge.nativeSelectionCommand(windowId, command)
        return true
    }

    // ---- Read path ----
    // The IME queries these to know the current text + selection
    // state so it can refine predictions, position candidates, etc.
    // Without these, Gboard's state-sync model can't trust its
    // internal view, and it re-sends commits / compositions
    // defensively (the ~155ms duplicate setComposingText pattern we
    // saw in the log). We answer from MainActivity's
    // `ImeTextState` mirror, which Rust pushes via JNI on every text
    // change.

    private fun mirror(): ImeTextState = shadowState

    /// Identifier of the gpui window this host's input flows
    /// into. Passed through to every `nativeIme*` JNI call so Rust
    /// can route the event to the right window's
    /// `PlatformInputHandler`. `0` = primary (MainActivity).
    override fun getTextBeforeCursor(n: Int, flags: Int): CharSequence? {
        val text = mirror().textBeforeCursor(n)
        Log.i(TAG, "IC.getTextBeforeCursor n=$n -> len=${text.length}")
        return text
    }

    override fun getTextAfterCursor(n: Int, flags: Int): CharSequence? {
        val text = mirror().textAfterCursor(n)
        Log.i(TAG, "IC.getTextAfterCursor n=$n -> len=${text.length}")
        return text
    }

    override fun getSelectedText(flags: Int): CharSequence? {
        val text = mirror().selectedText()
        if (text.isEmpty()) return null
        Log.i(TAG, "IC.getSelectedText -> len=${text.length}")
        return text
    }

    override fun getExtractedText(request: ExtractedTextRequest?, flags: Int): ExtractedText? {
        val state = mirror()
        Log.i(
            TAG,
            "IC.getExtractedText -> len=${state.text.length} sel=${state.selectionStart}..${state.selectionEnd}"
        )
        return state.extractedText()
    }

    companion object {
        private val nextConnectionId = AtomicLong(1L)
        private const val COMMAND_CUT = 1
        private const val COMMAND_COPY = 2
        private const val COMMAND_PASTE = 3
        private const val COMMAND_SELECT_ALL = 5
    }
}
