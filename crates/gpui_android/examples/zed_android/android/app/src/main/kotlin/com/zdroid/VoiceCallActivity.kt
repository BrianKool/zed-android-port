package com.zdroid

import android.app.Activity
import android.app.Dialog
import android.content.Context
import android.content.res.Configuration
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.Space
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

/** Touch-safe call surface hosted outside GameActivity's native input queue. */
class VoiceCallActivity : Activity() {
    private lateinit var stateLabel: TextView
    private lateinit var conversationContainer: LinearLayout
    private lateinit var muteButton: ImageView
    private lateinit var pauseButton: ImageView
    private lateinit var input: EditText
    private lateinit var voiceHero: View
    private lateinit var profileView: View
    private lateinit var heroTopSpacer: View
    private lateinit var heroBottomSpacer: View
    private lateinit var conversationScroll: ScrollView
    private var speechOutputEnabled = true
    private var showChat = false
    private var typingLayout = false
    private var voicePaused = false
    private var threadId = ""
    private var agentName = "Agent"
    private var modelName = "Current model"

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.R) {
                android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            } else {
                android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        WindowCompat.setDecorFitsSystemWindows(window, false)
        agentName = intent.getStringExtra(EXTRA_AGENT_NAME).orEmpty().ifBlank { "Agent" }
        modelName = intent.getStringExtra(EXTRA_MODEL_NAME).orEmpty().ifBlank { "Current model" }
        threadId = intent.getStringExtra(EXTRA_THREAD_ID).orEmpty()
        val content = buildContent()
        setContentView(content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { _, insets ->
            val statusBarTop = insets.getInsets(WindowInsetsCompat.Type.statusBars()).top
            val navigationBarBottom = insets.getInsets(WindowInsetsCompat.Type.navigationBars()).bottom
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            content.setPadding(
                0,
                statusBarTop,
                0,
                maxOf(navigationBarBottom, ime.bottom),
            )
            setTypingLayout(insets.isVisible(WindowInsetsCompat.Type.ime()))
            insets
        }
        ViewCompat.requestApplyInsets(content)
        Log.i(TAG, "call surface created for agent=$agentName model=$modelName")
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        agentName = intent.getStringExtra(EXTRA_AGENT_NAME).orEmpty().ifBlank { agentName }
        modelName = intent.getStringExtra(EXTRA_MODEL_NAME).orEmpty().ifBlank { modelName }
        threadId = intent.getStringExtra(EXTRA_THREAD_ID).orEmpty().ifBlank { threadId }
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
    fun updateConversation(bubbles: List<VoiceConversationStore.Bubble>) = runOnUiThread {
        if (!::conversationContainer.isInitialized) return@runOnUiThread
        val child = conversationScroll.getChildAt(0)
        val distanceFromBottom = child.height - conversationScroll.height - conversationScroll.scrollY
        val following = distanceFromBottom <= dp(56)
        val oldScroll = conversationScroll.scrollY
        var common = 0
        val sharedCount = minOf(conversationContainer.childCount, bubbles.size)
        while (common < sharedCount) {
            val view = conversationContainer.getChildAt(common) as? TextView ?: break
            if (view.tag != bubbles[common].id) break
            bindBubble(view, bubbles[common])
            common += 1
        }
        if (conversationContainer.childCount > common) {
            conversationContainer.removeViews(common, conversationContainer.childCount - common)
        }
        for (index in common until bubbles.size) {
            conversationContainer.addView(bubbleView(bubbles[index]), fullWidth(top = 8))
        }
        conversationScroll.post {
            if (following) conversationScroll.fullScroll(View.FOCUS_DOWN)
            else conversationScroll.scrollTo(0, oldScroll.coerceAtMost(conversationContainer.height))
        }
    }
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
        showChat = getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
            .getBoolean("show_chat", false)
        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setBackgroundColor(Color.BLACK)
        }
        val topBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(18), dp(12), dp(18), dp(4))
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
        heroTopSpacer = Space(this)
        content.addView(heroTopSpacer, LinearLayout.LayoutParams(1, 0, 1f))
        voiceHero = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            profileView = View(this@VoiceCallActivity).apply {
                contentDescription = "$agentName voice profile"
                background = GradientDrawable().apply {
                    shape = GradientDrawable.OVAL
                    colors = intArrayOf(Color.rgb(226, 228, 232), Color.rgb(132, 136, 145), Color.rgb(56, 59, 65))
                    gradientType = GradientDrawable.RADIAL_GRADIENT
                    gradientRadius = dp(145).toFloat()
                }
                setOnClickListener { VoiceConversationService.interruptSpeech(this@VoiceCallActivity) }
            }
            addView(profileView, LinearLayout.LayoutParams(dp(188), dp(188)))
            addView(label(agentName, 25f, Color.WHITE, Gravity.CENTER), fullWidth(top = 12))
            addView(label(modelName, 14f, Color.LTGRAY, Gravity.CENTER), fullWidth(top = 4))
        }
        content.addView(voiceHero, LinearLayout.LayoutParams(-1, -2))
        stateLabel = label("Starting microphone...", 16f, Color.rgb(116, 204, 168), Gravity.CENTER)
        content.addView(stateLabel, fullWidth(top = 10))
        val conversation = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(18), dp(8), dp(18), dp(8))
        }
        conversationContainer = conversation
        conversationScroll = ScrollView(this).apply { addView(conversation) }
        content.addView(
            conversationScroll,
            LinearLayout.LayoutParams(-1, 0, if (speechOutputEnabled && !showChat) 0.55f else 1f),
        )
        heroBottomSpacer = Space(this)
        content.addView(heroBottomSpacer, LinearLayout.LayoutParams(1, 0, 1f))

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
        content.addView(composer, LinearLayout.LayoutParams(-1, -2).apply {
            marginStart = dp(18)
            marginEnd = dp(18)
        })

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
        content.addView(actions, fullWidth(top = 10).apply {
            bottomMargin = dp(12)
        })
        return FrameLayout(this).apply { addView(content, FrameLayout.LayoutParams(-1, -1)) }
    }

    private fun setTypingLayout(typing: Boolean) {
        val compact = !speechOutputEnabled || showChat || typing ||
            resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE
        if (typingLayout == compact) return
        typingLayout = compact
        val avatarSize = if (compact) dp(72) else dp(188)
        profileView.layoutParams = (profileView.layoutParams as LinearLayout.LayoutParams).also {
            it.width = avatarSize
            it.height = avatarSize
        }
        heroTopSpacer.visibility = if (compact) View.GONE else View.VISIBLE
        heroBottomSpacer.visibility = if (compact) View.GONE else View.VISIBLE
        (conversationScroll.layoutParams as LinearLayout.LayoutParams).also {
            it.weight = if (compact) 1f else 0.55f
            conversationScroll.layoutParams = it
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
        val speechEnabled = preferences.getBoolean("speech_output", true)
        val chatVisible = preferences.getBoolean("show_chat", false)
        val speakerEnabled = preferences.getBoolean("speaker", true)
        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply {
                shape = GradientDrawable.RECTANGLE
                setColor(Color.rgb(37, 38, 37))
                setStroke(dp(1), Color.rgb(88, 87, 79))
                cornerRadius = dp(8).toFloat()
            }
        }
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(20), dp(14), dp(10), dp(10))
        }
        header.addView(label("Voice settings", 20f, SETTINGS_TEXT, Gravity.START), LinearLayout.LayoutParams(0, -2, 1f))
        header.addView(iconAction(android.R.drawable.ic_menu_close_clear_cancel, "Close settings", Color.TRANSPARENT) {
            dialog.dismiss()
        }, LinearLayout.LayoutParams(dp(48), dp(48)))
        panel.addView(header, LinearLayout.LayoutParams(-1, -2))
        panel.addView(settingsDivider(), LinearLayout.LayoutParams(-1, dp(1)))

        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(8), dp(20), dp(18))
        }
        body.addView(settingsSectionTitle("Language"), fullWidth(top = 8))
        val traditionalChinese = settingsCheckBox("Traditional Chinese", "zh-TW" in selected)
        val english = settingsCheckBox("English", "en-US" in selected)
        body.addView(traditionalChinese, fullWidth(top = 4))
        body.addView(english, fullWidth())

        body.addView(settingsSectionTitle("AI voice output"), fullWidth(top = 18))
        val spokenResponses = settingsSwitch("Spoken responses", speechEnabled)
        val showConversation = settingsSwitch("Show conversation", chatVisible)
        body.addView(spokenResponses, fullWidth(top = 4))
        body.addView(showConversation, fullWidth())

        body.addView(settingsSectionTitle("Audio output"), fullWidth(top = 18))
        val audioGroup = RadioGroup(this).apply { orientation = RadioGroup.VERTICAL }
        val speaker = settingsRadioButton("Speaker", speakerEnabled)
        val earpiece = settingsRadioButton("Earpiece", !speakerEnabled)
        audioGroup.addView(speaker, LinearLayout.LayoutParams(-1, dp(48)))
        audioGroup.addView(earpiece, LinearLayout.LayoutParams(-1, dp(48)))
        body.addView(audioGroup, fullWidth(top = 4))

        val scroll = ScrollView(this).apply {
            isFillViewport = true
            addView(body, ViewGroup.LayoutParams(-1, -2))
        }
        panel.addView(scroll, LinearLayout.LayoutParams(-1, 0, 1f))
        panel.addView(settingsDivider(), LinearLayout.LayoutParams(-1, dp(1)))

        val footer = LinearLayout(this).apply {
            gravity = Gravity.END or Gravity.CENTER_VERTICAL
            setPadding(dp(14), dp(10), dp(14), dp(12))
        }
        footer.addView(settingsCommand("Cancel", false) { dialog.dismiss() }, LinearLayout.LayoutParams(-2, dp(46)))
        footer.addView(settingsCommand("Apply", true) {
            if (!traditionalChinese.isChecked && !english.isChecked) {
                Toast.makeText(this, "Select at least one language", Toast.LENGTH_SHORT).show()
                return@settingsCommand
            }
            selected.clear()
            if (traditionalChinese.isChecked) selected.add("zh-TW")
            if (english.isChecked) selected.add("en-US")
            val useSpeaker = speaker.isChecked
            preferences.edit()
                .putStringSet("languages", selected)
                .putBoolean("speech_output", spokenResponses.isChecked)
                .putBoolean("show_chat", showConversation.isChecked)
                .putBoolean("speaker", useSpeaker)
                .apply()
            VoiceConversationService.setLanguages(this, selected)
            VoiceConversationService.setSpeechOutput(this, spokenResponses.isChecked)
            VoiceConversationService.setAudioOutput(this, useSpeaker)
            dialog.dismiss()
            recreate()
        }, LinearLayout.LayoutParams(-2, dp(46)).apply { marginStart = dp(8) })
        panel.addView(footer, LinearLayout.LayoutParams(-1, -2))

        dialog.setContentView(panel)
        dialog.setCanceledOnTouchOutside(true)
        dialog.show()
        dialog.window?.apply {
            setBackgroundDrawableResource(android.R.color.transparent)
            addFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND)
            attributes = attributes.apply {
                width = ViewGroup.LayoutParams.MATCH_PARENT
                height = (resources.displayMetrics.heightPixels * 0.82f).toInt()
                dimAmount = 0.72f
                gravity = Gravity.CENTER
            }
            decorView.setPadding(dp(18), 0, dp(18), 0)
            if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.S) {
                addFlags(WindowManager.LayoutParams.FLAG_BLUR_BEHIND)
                attributes = attributes.apply { blurBehindRadius = dp(18) }
            }
        }
    }

    private fun settingsSectionTitle(text: String) = label(text, 13f, SETTINGS_ACCENT, Gravity.START).apply {
        setTypeface(Typeface.DEFAULT, Typeface.BOLD)
    }

    private fun settingsCheckBox(text: String, checked: Boolean) = CheckBox(this).apply {
        this.text = text
        isChecked = checked
        setTextColor(SETTINGS_TEXT)
        textSize = 16f
        buttonTintList = ColorStateList.valueOf(SETTINGS_ACCENT)
        gravity = Gravity.CENTER_VERTICAL
    }

    @Suppress("UseSwitchCompatOrMaterialCode")
    private fun settingsSwitch(text: String, checked: Boolean) = Switch(this).apply {
        this.text = text
        isChecked = checked
        setTextColor(SETTINGS_TEXT)
        textSize = 16f
        gravity = Gravity.CENTER_VERTICAL
        showText = false
        buttonTintList = ColorStateList.valueOf(SETTINGS_ACCENT)
    }

    private fun settingsRadioButton(text: String, checked: Boolean) = RadioButton(this).apply {
        this.text = text
        isChecked = checked
        setTextColor(SETTINGS_TEXT)
        textSize = 16f
        buttonTintList = ColorStateList.valueOf(SETTINGS_ACCENT)
        gravity = Gravity.CENTER_VERTICAL
    }

    private fun settingsDivider() = View(this).apply { setBackgroundColor(Color.rgb(78, 78, 72)) }

    private fun settingsCommand(text: String, primary: Boolean, click: () -> Unit) = TextView(this).apply {
        this.text = text
        textSize = 15f
        setTextColor(if (primary) Color.rgb(35, 35, 32) else SETTINGS_TEXT)
        gravity = Gravity.CENTER
        setPadding(dp(18), 0, dp(18), 0)
        background = rounded(if (primary) SETTINGS_ACCENT else Color.rgb(54, 55, 53), 6)
        isClickable = true
        isFocusable = true
        setOnClickListener { click() }
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

    private fun bubbleView(bubble: VoiceConversationStore.Bubble): View {
        return TextView(this).apply {
            textSize = 16f
            setPadding(dp(14), dp(11), dp(14), dp(11))
            bindBubble(this, bubble)
        }
    }

    private fun bindBubble(view: TextView, bubble: VoiceConversationStore.Bubble) {
        val user = bubble.role == VoiceConversationStore.Role.USER
        view.tag = bubble.id
        view.text = bubble.text
        view.setTextColor(if (bubble.partial) Color.LTGRAY else Color.WHITE)
        view.setTypeface(Typeface.DEFAULT, if (bubble.partial) Typeface.ITALIC else Typeface.NORMAL)
        view.gravity = if (user) Gravity.END else Gravity.START
        view.background = rounded(
            if (user) Color.rgb(40, 84, 68) else Color.rgb(39, 41, 46),
            12,
        )
        view.contentDescription = when {
            bubble.partial -> "Partial transcript"
            user -> "You"
            else -> "Agent"
        }
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
        private val SETTINGS_TEXT = Color.rgb(239, 232, 202)
        private val SETTINGS_ACCENT = Color.rgb(218, 198, 120)
        const val EXTRA_AGENT_NAME = "agent_name"
        const val EXTRA_MODEL_NAME = "model_name"
        const val EXTRA_THREAD_ID = "thread_id"
    }
}
