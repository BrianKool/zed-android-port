package com.zdroid

import android.text.SpannableStringBuilder
import android.util.Log
import android.view.inputmethod.ExtractedText

/// Kotlin-side mirror of the gpui editor's text state that the IME
/// can query synchronously. Updated by Rust after every commit /
/// compose / delete via `MainActivity.updateImeTextState`.
///
/// We mirror only a window of text around the cursor (~256 chars
/// each side) rather than the full document, because the IME never
/// needs more than that for prediction / context, and a Zed buffer
/// can be megabytes large.
///
/// All indices are UTF-16 code-unit offsets in the FULL document,
/// not in `text`. Use `relInWindow()` to convert an absolute index to
/// a position within `text` for substring extraction.
data class ImeTextState(
    val text: String,
    val windowStart: Int,
    val selectionStart: Int,
    val selectionEnd: Int,
    val composingStart: Int, // -1 = no active composition
    val composingEnd: Int,   // -1 = no active composition
    val revision: Long = 0L,
) {
    /// Convert an absolute UTF-16 offset to an index within `text`.
    /// Returns null if the offset falls outside the mirrored window
    /// (caller should report the empty string or fall back gracefully).
    private fun relInWindow(absolute: Int): Int? {
        val rel = absolute - windowStart
        return if (rel in 0..text.length) rel else null
    }

    /// `InputConnection.getTextBeforeCursor(n)` — n UTF-16 units of
    /// text BEFORE the cursor that is NOT part of the active
    /// composition. Per Android contract (InputConnection
    /// Javadoc): "This will not include any currently composing
    /// text".
    ///
    /// Subtle: when composition is active, the buffer already
    /// contains the composing letters (replace_and_mark inserted
    /// them with a marked highlight). Cursor sits at composingEnd.
    /// If we naively returned text up to selectionStart we'd hand
    /// Gboard the composing letters as part of the prior context —
    /// Gboard then treats them as "the word the user is currently
    /// typing" plus surrounding buffer, and on the next keystroke
    /// rebuilds a `setComposingText` that includes the surrounding
    /// buffer too. That's the "editor vomits a garbage paste"
    /// regression. Truncating at composingStart fixes it.
    fun textBeforeCursor(n: Int): CharSequence {
        val boundary = if (composingStart in 0 until selectionStart) composingStart else selectionStart
        val end = relInWindow(boundary) ?: return ""
        val start = (end - n).coerceAtLeast(0)
        return text.substring(start, end)
    }

    /// `InputConnection.getTextAfterCursor(n)` — n UTF-16 units of
    /// text AFTER the cursor that is NOT part of the active
    /// composition. Same rationale as [textBeforeCursor].
    fun textAfterCursor(n: Int): CharSequence {
        val boundary = if (composingEnd > selectionEnd) composingEnd else selectionEnd
        val start = relInWindow(boundary) ?: return ""
        val end = (start + n).coerceAtMost(text.length)
        return text.substring(start, end)
    }

    /// `InputConnection.getSelectedText()` — text between selection
    /// start/end. Empty when there's no selection (cursor only).
    fun selectedText(): CharSequence {
        if (selectionEnd <= selectionStart) return ""
        val start = relInWindow(selectionStart) ?: return ""
        val end = relInWindow(selectionEnd) ?: return ""
        return text.substring(start, end)
    }

    /// `InputConnection.getExtractedText()` — full snapshot the IME
    /// uses for fullscreen extract mode AND for sanity checks.
    /// Returning the mirrored window is enough; IMEs handle short
    /// snapshots gracefully (Gboard, Swiftkey, Samsung all do).
    fun extractedText(): ExtractedText {
        val out = ExtractedText()
        out.text = SpannableStringBuilder(text)
        out.startOffset = windowStart
        // Selection start/end in ExtractedText are RELATIVE to startOffset.
        out.selectionStart = (selectionStart - windowStart).coerceIn(0, text.length)
        out.selectionEnd = (selectionEnd - windowStart).coerceIn(0, text.length)
        out.partialStartOffset = -1
        out.partialEndOffset = -1
        out.flags = 0
        return out
    }

    fun withComposingText(value: String, newCursorPosition: Int, revision: Long): ImeTextState =
        replaceImeRange(value, newCursorPosition, composing = true, revision = revision)

    fun withCommittedText(value: String, newCursorPosition: Int, revision: Long): ImeTextState =
        replaceImeRange(value, newCursorPosition, composing = false, revision = revision)

    fun withoutComposition(revision: Long): ImeTextState = copy(
        composingStart = -1,
        composingEnd = -1,
        revision = revision,
    )

    fun withSelection(start: Int, end: Int, revision: Long): ImeTextState {
        val documentEnd = windowStart + text.length
        val clippedStart = start.coerceIn(windowStart, documentEnd)
        val clippedEnd = end.coerceIn(windowStart, documentEnd)
        return copy(
            selectionStart = minOf(clippedStart, clippedEnd),
            selectionEnd = maxOf(clippedStart, clippedEnd),
            revision = revision,
        )
    }

    fun withComposingRegion(start: Int, end: Int, revision: Long): ImeTextState {
        val documentEnd = windowStart + text.length
        val clippedStart = start.coerceIn(windowStart, documentEnd)
        val clippedEnd = end.coerceIn(windowStart, documentEnd)
        return copy(
            composingStart = minOf(clippedStart, clippedEnd),
            composingEnd = maxOf(clippedStart, clippedEnd),
            revision = revision,
        )
    }

    fun deletingSurroundingText(beforeLength: Int, afterLength: Int, revision: Long): ImeTextState {
        val startAbsolute = (selectionStart - beforeLength.coerceAtLeast(0)).coerceAtLeast(windowStart)
        val endAbsolute = (selectionEnd + afterLength.coerceAtLeast(0))
            .coerceAtMost(windowStart + text.length)
        return replaceAbsoluteRange(startAbsolute, endAbsolute, "", 1, false, revision)
    }

    private fun replaceImeRange(
        value: String,
        newCursorPosition: Int,
        composing: Boolean,
        revision: Long,
    ): ImeTextState {
        val hasComposition = composingStart >= windowStart && composingEnd >= composingStart
        val start = if (hasComposition) composingStart else selectionStart
        val end = if (hasComposition) composingEnd else selectionEnd
        return replaceAbsoluteRange(start, end, value, newCursorPosition, composing, revision)
    }

    private fun replaceAbsoluteRange(
        startAbsolute: Int,
        endAbsolute: Int,
        value: String,
        newCursorPosition: Int,
        composing: Boolean,
        revision: Long,
    ): ImeTextState {
        val start = relInWindow(startAbsolute) ?: return copy(revision = revision)
        val end = relInWindow(endAbsolute) ?: return copy(revision = revision)
        val updated = buildString(text.length - (end - start) + value.length) {
            append(text, 0, start)
            append(value)
            append(text, end, text.length)
        }
        val insertedEnd = startAbsolute + value.length
        val cursor = if (newCursorPosition > 0) {
            insertedEnd + newCursorPosition - 1
        } else {
            startAbsolute + newCursorPosition
        }.coerceIn(windowStart, windowStart + updated.length)
        return ImeTextState(
            text = updated,
            windowStart = windowStart,
            selectionStart = cursor,
            selectionEnd = cursor,
            composingStart = if (composing) startAbsolute else -1,
            composingEnd = if (composing) insertedEnd else -1,
            revision = revision,
        )
    }

    companion object {
        const val TAG = "zdroid_ime"

        /// Stand-in when Rust hasn't pushed any state yet (e.g., the
        /// editor isn't focused). Lets the IME's queries return empty
        /// strings rather than throwing.
        val EMPTY: ImeTextState =
            ImeTextState(text = "", windowStart = 0, selectionStart = 0, selectionEnd = 0, composingStart = -1, composingEnd = -1)
    }
}
