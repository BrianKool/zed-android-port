package com.zdroid

import android.app.Activity
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.os.Build
import android.view.ActionMode
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import kotlin.math.max
import kotlin.math.min

/** Native handles and floating actions for selection in gpui's custom surface. */
class SelectionOverlayController(
    private val activity: Activity,
    private val windowId: Long,
) {
    private val density = activity.resources.displayMetrics.density
    private val handleSize = (32f * density).toInt()
    private val startHandle = SelectionHandleView(activity, 0)
    private val endHandle = SelectionHandleView(activity, 1)
    private var actionMode: ActionMode? = null
    private var startX = 0f
    private var startY = 0f
    private var endX = 0f
    private var endY = 0f
    private var visible = false

    init {
        val params = FrameLayout.LayoutParams(handleSize, handleSize)
        activity.addContentView(startHandle, FrameLayout.LayoutParams(params))
        activity.addContentView(endHandle, FrameLayout.LayoutParams(params))
        startHandle.visibility = View.GONE
        endHandle.visibility = View.GONE
    }

    fun update(show: Boolean, sx: Float, sy: Float, ex: Float, ey: Float) {
        visible = show
        if (!show) {
            startHandle.visibility = View.GONE
            endHandle.visibility = View.GONE
            actionMode?.finish()
            actionMode = null
            return
        }

        startX = sx
        startY = sy
        endX = ex
        endY = ey
        place(startHandle, sx, sy)
        place(endHandle, ex, ey)
        startHandle.visibility = View.VISIBLE
        endHandle.visibility = View.VISIBLE

        if (actionMode == null) {
            actionMode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                activity.startActionMode(actionModeCallback, ActionMode.TYPE_FLOATING)
            } else {
                activity.startActionMode(actionModeCallback)
            }
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            actionMode?.invalidateContentRect()
        }
    }

    fun destroy() {
        visible = false
        actionMode?.finish()
        actionMode = null
        (startHandle.parent as? ViewGroup)?.removeView(startHandle)
        (endHandle.parent as? ViewGroup)?.removeView(endHandle)
    }

    private fun place(view: View, x: Float, y: Float) {
        val params = view.layoutParams as FrameLayout.LayoutParams
        params.leftMargin = (x - handleSize / 2f).toInt()
        params.topMargin = y.toInt()
        view.layoutParams = params
    }

    private val actionModeCallback = object : ActionMode.Callback2() {
        override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
            addAction(menu, COMMAND_CUT, "Cut")
            addAction(menu, COMMAND_COPY, "Copy")
            addAction(menu, COMMAND_PASTE, "Paste")
            addAction(menu, COMMAND_MORE, "More")
            return true
        }

        override fun onPrepareActionMode(mode: ActionMode, menu: Menu): Boolean = false

        override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
            if (item.itemId !in COMMAND_CUT..COMMAND_MORE) return false
            if (item.itemId == COMMAND_MORE) {
                mode.finish()
            }
            NativeBridge.nativeSelectionCommand(windowId, item.itemId)
            return true
        }

        override fun onDestroyActionMode(mode: ActionMode) {
            if (actionMode === mode) actionMode = null
        }

        override fun onGetContentRect(mode: ActionMode, view: View, outRect: Rect) {
            val padding = (8f * density).toInt()
            outRect.set(
                min(startX, endX).toInt() - padding,
                min(startY, endY).toInt() - padding,
                max(startX, endX).toInt() + padding,
                max(startY, endY).toInt() + padding,
            )
        }

        private fun addAction(menu: Menu, id: Int, title: String) {
            menu.add(Menu.NONE, id, Menu.NONE, title)
                .setShowAsAction(MenuItem.SHOW_AS_ACTION_ALWAYS)
        }
    }

    private inner class SelectionHandleView(activity: Activity, private val endpoint: Int) :
        View(activity) {
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(66, 133, 244) }
        private var grabX = 0f
        private var grabY = 0f

        override fun onDraw(canvas: Canvas) {
            val cx = width / 2f
            val stemBottom = 10f * density
            canvas.drawRect(cx - density, 0f, cx + density, stemBottom, paint)
            canvas.drawCircle(cx, stemBottom + 6f * density, 6f * density, paint)
        }

        override fun onTouchEvent(event: MotionEvent): Boolean {
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    grabX = event.x
                    grabY = event.y
                    parent.requestDisallowInterceptTouchEvent(true)
                    return true
                }
                MotionEvent.ACTION_MOVE, MotionEvent.ACTION_UP -> {
                    val decorLocation = IntArray(2)
                    activity.window.decorView.getLocationOnScreen(decorLocation)
                    val x = event.rawX - decorLocation[0] - grabX + width / 2f
                    val y = event.rawY - decorLocation[1] - grabY
                    NativeBridge.nativeSelectionAdjust(
                        this@SelectionOverlayController.windowId,
                        endpoint,
                        x,
                        y,
                    )
                    if (event.actionMasked == MotionEvent.ACTION_UP) {
                        parent.requestDisallowInterceptTouchEvent(false)
                    }
                    return true
                }
                MotionEvent.ACTION_CANCEL -> {
                    parent.requestDisallowInterceptTouchEvent(false)
                    return true
                }
            }
            return false
        }
    }

    companion object {
        private const val COMMAND_CUT = 1
        private const val COMMAND_COPY = 2
        private const val COMMAND_PASTE = 3
        private const val COMMAND_MORE = 4
    }
}
