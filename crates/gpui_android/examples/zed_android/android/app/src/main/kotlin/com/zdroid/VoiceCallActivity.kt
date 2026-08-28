package com.zdroid

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.Space
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

/** Touch-safe call surface hosted outside GameActivity's native input queue. */
class VoiceCallActivity : Activity() {
    private lateinit var stateLabel: TextView
    private lateinit var transcriptLabel: TextView
    private lateinit var responseLabel: TextView
    private lateinit var muteButton: ImageView
    private lateinit var pauseButton: ImageView
    private lateinit var input: EditText
    private lateinit var voiceHero: View
    private lateinit var heroTopSpacer: View
    private lateinit var heroBottomSpacer: View
    private lateinit var conversationScroll: ScrollView
    private var speechOutputEnabled = true
    private var typingLayout = false
    private var voicePaused = false
    private var agentName = "Agent"
    private var modelName = "Current model"

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        agentName = intent.getStringExtra(EXTRA_AGENT_NAME).orEmpty().ifBlank { "Agent" }
        modelName = intent.getStringExtra(EXTRA_MODEL_NAME).orEmpty().ifBlank { "Current model" }
        val content = buildContent()
        setContentView(content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { _, insets ->
            setTypingLayout(insets.isVisible(WindowInsetsCompat.Type.ime()))
            insets
        }
        Log.i(TAG, "call surface created for agent=$agentName model=$modelName")
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        agentName = intent.getStringExtra(EXTRA_AGENT_NAME).orEmpty().ifBlank { agentName }
        modelName = intent.getStringExtra(EXTRA_MODEL_NAME).orEmpty().ifBlank { modelName }
    }

    override fun onStart() {
        super.onStart()
        VoiceConversationService.attachCallActivity(this)
    }

    override fun onStop() {
        VoiceConversationService.attachCallActivity(null)
        super.onStop()
    }

    fun updateState(value: String) = runOnUiThread { stateLabel.text = value }
    fun updateTranscript(value: String) = runOnUiThread { transcriptLabel.text = value }
    fun updateResponse(value: String) = runOnUiThread { responseLabel.text = value }
    fun updateMuteState(muted: Boolean) = runOnUiThread {
        muteButton.setImageResource(if (muted) R.drawable.ic_voice_mic_off else R.drawable.ic_voice_mic)
        muteButton.imageTintList = ColorStateList.valueOf(if (muted) Color.rgb(220, 64, 72) else Color.WHITE)
        muteButton.contentDescription = if (muted) "Unmute" else "Mute"
    }
    fun updatePauseState(paused: Boolean) = runOnUiThread {
        voicePaused = paused
        pauseButton.setImageResource(if (paused) R.drawable.ic_voice_play else R.drawable.ic_voice_pause)
        pauseButton.contentDescription = if (paused) "Resume voice conversation" else "Pause voice conversation"
    }

    fun finishCall() = runOnUiThread { finish() }

    private fun buildContent(): View {
        speechOutputEnabled = getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
            .getBoolean("speech_output", true)
        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(22), dp(24), dp(22), dp(22))
            setBackgroundColor(Color.BLACK)
        }
        val topBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        topBar.addView(Space(this), LinearLayout.LayoutParams(0, 1, 1f))
        topBar.addView(
            iconAction(R.drawable.ic_voice_settings, "Voice settings", Color.rgb(42, 44, 49)) {
                Log.i(TAG, "settings clicked")
                showSettings()
            },
            LinearLayout.LayoutParams(dp(52), dp(52)),
        )
        content.addView(topBar, LinearLayout.LayoutParams(-1, -2))
        if (speechOutputEnabled) {
            heroTopSpacer = Space(this)
            content.addView(heroTopSpacer, LinearLayout.LayoutParams(1, 0, 1f))
            voiceHero = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER_HORIZONTAL
                addView(View(this@VoiceCallActivity).apply {
                    contentDescription = "$agentName voice profile"
                    background = GradientDrawable().apply {
                        shape = GradientDrawable.OVAL
                        colors = intArrayOf(Color.rgb(226, 228, 232), Color.rgb(132, 136, 145), Color.rgb(56, 59, 65))
                        gradientType = GradientDrawable.RADIAL_GRADIENT
                        gradientRadius = dp(145).toFloat()
                    }
                    setOnClickListener { VoiceConversationService.interruptSpeech(this@VoiceCallActivity) }
                }, LinearLayout.LayoutParams(dp(188), dp(188)))
                addView(label(agentName, 25f, Color.WHITE, Gravity.CENTER), fullWidth(top = 20))
                addView(label(modelName, 14f, Color.LTGRAY, Gravity.CENTER), fullWidth(top = 4))
            }
            content.addView(voiceHero, LinearLayout.LayoutParams(-1, -2))
        } else {
            heroTopSpacer = Space(this)
            voiceHero = Space(this)
            content.addView(label(agentName, 20f, Color.WHITE, Gravity.START), fullWidth(top = 6))
        }
        stateLabel = label("Starting microphone...", 16f, Color.rgb(116, 204, 168), Gravity.CENTER)
        content.addView(stateLabel, fullWidth(top = 10))
        val conversation = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(8), 0, dp(8))
        }
        transcriptLabel = label("", 17f, Color.WHITE, if (speechOutputEnabled) Gravity.CENTER else Gravity.START)
        conversation.addView(transcriptLabel, fullWidth(top = 10))
        responseLabel = label("", 16f, Color.LTGRAY, if (speechOutputEnabled) Gravity.CENTER else Gravity.START)
        conversation.addView(responseLabel, fullWidth(top = 12))
        conversationScroll = ScrollView(this).apply { addView(conversation) }
        content.addView(conversationScroll, LinearLayout.LayoutParams(-1, 0, if (speechOutputEnabled) 0.55f else 1f))
        if (speechOutputEnabled) {
            heroBottomSpacer = Space(this)
            content.addView(heroBottomSpacer, LinearLayout.LayoutParams(1, 0, 1f))
        } else {
            heroBottomSpacer = Space(this)
        }

        val composer = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(8), dp(7), dp(8), dp(7))
            background = rounded(Color.rgb(31, 32, 35), 28)
        }
        composer.addView(TextView(this).apply {
            text = "+"
            textSize = 24f
            setTextColor(Color.WHITE)
            gravity = Gravity.CENTER
            isClickable = true
            isFocusable = true
            setOnClickListener { showContextMenu(this) }
        }, LinearLayout.LayoutParams(dp(48), dp(48)))
        input = EditText(this).apply {
            hint = "Message $agentName"
            setHintTextColor(Color.GRAY)
            setTextColor(Color.WHITE)
            textSize = 16f
            maxLines = 4
            background = null
            setPadding(dp(10), 0, dp(8), 0)
            setOnFocusChangeListener { _, focused ->
                if (focused) setTypingLayout(true)
            }
        }
        composer.addView(input, LinearLayout.LayoutParams(0, -2, 1f))
        composer.addView(iconAction(R.drawable.ic_voice_send, "Send message", Color.rgb(42, 44, 49)) {
            val text = input.text.toString().trim()
            if (text.isNotEmpty()) {
                Log.i(TAG, "typed prompt submitted (${text.length} chars)")
                input.text.clear()
                input.clearFocus()
                (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(input.windowToken, 0)
                VoiceConversationService.submitTyped(this, text)
            }
        }, LinearLayout.LayoutParams(dp(48), dp(48)))
        content.addView(composer, LinearLayout.LayoutParams(-1, -2))

        val actions = LinearLayout(this).apply { gravity = Gravity.CENTER }
        pauseButton = iconAction(R.drawable.ic_voice_pause, "Pause voice conversation", Color.rgb(42, 44, 49)) {
            Log.i(TAG, "pause/resume clicked")
            if (voicePaused) {
                VoiceConversationService.resume(this)
            } else {
                VoiceConversationService.pause(this)
            }
        }
        actions.addView(pauseButton, LinearLayout.LayoutParams(dp(58), dp(58)).apply { marginEnd = dp(18) })
        muteButton = iconAction(R.drawable.ic_voice_mic, "Mute", Color.rgb(42, 44, 49)) {
            Log.i(TAG, "mute clicked")
            VoiceConversationService.toggleMute(this)
        }
        actions.addView(muteButton, LinearLayout.LayoutParams(dp(58), dp(58)).apply { marginEnd = dp(18) })
        actions.addView(iconAction(R.drawable.ic_voice_stop, "End voice conversation", Color.rgb(205, 54, 64)) {
            Log.i(TAG, "end-call clicked")
            VoiceConversationService.stop(this)
        }, LinearLayout.LayoutParams(dp(58), dp(58)))
        content.addView(actions, fullWidth(top = 10))
        return FrameLayout(this).apply { addView(content, FrameLayout.LayoutParams(-1, -1)) }
    }

    private fun setTypingLayout(typing: Boolean) {
        val compact = !speechOutputEnabled || typing
        if (typingLayout == compact) return
        typingLayout = compact
        if (speechOutputEnabled) {
            voiceHero.visibility = if (compact) View.GONE else View.VISIBLE
            heroTopSpacer.visibility = if (compact) View.GONE else View.VISIBLE
            heroBottomSpacer.visibility = if (compact) View.GONE else View.VISIBLE
            transcriptLabel.gravity = if (compact) Gravity.START else Gravity.CENTER
            responseLabel.gravity = if (compact) Gravity.START else Gravity.CENTER
            (conversationScroll.layoutParams as LinearLayout.LayoutParams).also {
                it.weight = if (compact) 1f else 0.55f
                conversationScroll.layoutParams = it
            }
        }
        if (compact) conversationScroll.post { conversationScroll.fullScroll(View.FOCUS_DOWN) }
    }

    private fun showContextMenu(anchor: View) {
        PopupMenu(this, anchor).apply {
            menu.add("Return to Agent chat for files and context").setOnMenuItemClickListener { finish(); true }
            menu.add("Use Browser").setOnMenuItemClickListener {
                VoiceConversationService.submitTyped(this@VoiceCallActivity, "Use Phone Use to open and operate Android Chrome. ")
                true
            }
            menu.add("Phone Use settings").setOnMenuItemClickListener {
                startActivity(android.content.Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS)); true
            }
            show()
        }
    }

    private fun showSettings() {
        val preferences = getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
        val selected = (preferences.getStringSet("languages", setOf("zh-TW", "en-US"))
            ?: setOf("zh-TW", "en-US")).toMutableSet()
        val labels = arrayOf("繁體中文", "English")
        val codes = arrayOf("zh-TW", "en-US")
        val speechEnabled = preferences.getBoolean("speech_output", true)
        AlertDialog.Builder(this)
            .setTitle("Voice settings")
            .setMultiChoiceItems(labels, BooleanArray(codes.size) { codes[it] in selected }) { _, which, checked ->
                if (checked) selected.add(codes[which]) else if (selected.size > 1) selected.remove(codes[which])
            }
            .setPositiveButton("Apply") { _, _ ->
                preferences.edit().putStringSet("languages", selected).apply()
                VoiceConversationService.setLanguages(this, selected)
            }
            .setNeutralButton("Audio output") { _, _ ->
                val speaker = !preferences.getBoolean("speaker", true)
                preferences.edit().putBoolean("speaker", speaker).apply()
                VoiceConversationService.setAudioOutput(this, speaker)
                Toast.makeText(this, if (speaker) "Speaker" else "Earpiece", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton(if (speechEnabled) "Turn voice output off" else "Turn voice output on") { _, _ ->
                val enabled = !speechEnabled
                preferences.edit().putBoolean("speech_output", enabled).apply()
                VoiceConversationService.setSpeechOutput(this, enabled)
                recreate()
            }
            .show()
    }

    private fun iconAction(icon: Int, description: String, color: Int, click: () -> Unit) = ImageView(this).apply {
        setImageResource(icon)
        imageTintList = ColorStateList.valueOf(Color.WHITE)
        contentDescription = description
        setPadding(dp(14), dp(14), dp(14), dp(14))
        background = rounded(color, 28)
        isClickable = true
        isFocusable = true
        setOnClickListener { click() }
    }

    private fun label(text: String, size: Float, color: Int, gravityValue: Int) = TextView(this).apply {
        this.text = text; textSize = size; setTextColor(color); gravity = gravityValue
    }
    private fun rounded(color: Int, radius: Int) = GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE; setColor(color); cornerRadius = dp(radius).toFloat()
    }
    private fun fullWidth(top: Int = 0) = LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(top) }
    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    companion object {
        private const val TAG = "ZdroidVoiceUI"
        const val EXTRA_AGENT_NAME = "agent_name"
        const val EXTRA_MODEL_NAME = "model_name"
    }
}
