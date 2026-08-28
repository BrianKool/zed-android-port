package com.zdroid

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.icu.text.Transliterator
import android.media.AudioManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.text.SpannableString
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.lang.ref.WeakReference
import java.util.Locale
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

/** Keeps a hands-free Agent conversation alive while the app is backgrounded or locked. */
class VoiceConversationService : Service(), RecognitionListener, TextToSpeech.OnInitListener {
    private enum class VoiceMode { ACTIVE, WAKE_ONLY, ENDED }

    private var recognizer: SpeechRecognizer? = null
    private var wakeRecognizerIsOnDevice = false
    private var realtimeCapture: RealtimeVoiceCapture? = null
    private var recognizerAudioSource: ParcelFileDescriptor? = null
    private var usesRealtimeCapture = false
    private var realtimeCaptureDisabledForSession = false
    private var realtimeSessionReady = false
    private var tts: TextToSpeech? = null
    private var ttsReady = false
    private var waitingForAgent = false
    private var muted = false
    private var voiceMode = VoiceMode.ACTIVE
    private var isListening = false
    private var speakerEnabled = true
    private var speechOutputEnabled = true
    private var languages = linkedSetOf("zh-TW", "en-US")
    private var lastStreamingResponse = ""
    private val streamingBuffer = StringBuilder()
    private var latestPartialTranscript = ""
    private var lastSubmittedTranscript = ""
    private var consecutiveRecognizerErrors = 0
    private var lastPublishedState = ""
    private var lastNotificationAt = 0L
    private var speechStartedAt = 0L
    private var streamingFlushScheduled = false
    private val streamingHandler = Handler(Looper.getMainLooper())
    private val inputWorker: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "ZdroidVoiceInput").apply { priority = Thread.NORM_PRIORITY }
    }
    private val outputWorker: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "ZdroidVoiceOutput").apply { priority = Thread.NORM_PRIORITY }
    }
    private val orchestrator = VoiceOrchestrator()
    private val speechScheduler = VoiceSpeechScheduler()
    private val endpointing = DynamicEndpointing()
    private val latency = VoiceLatencyTelemetry(TAG)
    private val vadBenchmark = MobileVadBenchmark(RealtimeVoiceCapture.SAMPLE_RATE) { elapsed, rms, threshold ->
        Log.i(
            TAG,
            "vad-benchmark candidate=${elapsed}ms rms=$rms threshold=$threshold",
        )
    }
    private val flushStreamingSpeech = Runnable {
        streamingFlushScheduled = false
        drainStreamingBuffer(force = true)
    }
    private val finalizePartialTurn = Runnable {
        val transcript = latestPartialTranscript.trim()
        if (transcript.isNotEmpty() && transcript != lastSubmittedTranscript && shouldListen()) {
            Log.i(TAG, "finalizing partial transcript after silence (${transcript.length} chars)")
            submitRecognizedTranscript(transcript, confidence = null)
        }
    }
    private val resumeAfterFalseAlarm = Runnable {
        if (isSoftInterruptionActive()) resumeSoftInterruptedOutput()
    }
    private val restartListening = Runnable {
        if (shouldListen()) startListening()
    }
    private val inputSimplifiedToTraditional by lazy {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            runCatching { Transliterator.getInstance("Simplified-Traditional") }.getOrNull()
        } else {
            null
        }
    }
    private val outputSimplifiedToTraditional by lazy {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            runCatching { Transliterator.getInstance("Simplified-Traditional") }.getOrNull()
        } else {
            null
        }
    }
    private var active = true
    private var foregroundStarted = false
    private var callStartedAt = 0L
    private var audioFocusRequest: AudioFocusRequest? = null

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "voice service created sdk=${Build.VERSION.SDK_INT}")
        ensureChannel(this)
        val preferences = getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
        speakerEnabled = preferences.getBoolean("speaker", true)
        speechOutputEnabled = preferences.getBoolean("speech_output", true)
        languages = (preferences.getStringSet("languages", setOf("zh-TW", "en-US"))
            ?: setOf("zh-TW", "en-US")).toCollection(linkedSetOf())
        callStartedAt = preferences.getLong("call_started_at", 0L).takeIf { it > 0L }
            ?: System.currentTimeMillis().also {
                preferences.edit().putLong("call_started_at", it).apply()
            }
        tts = TextToSpeech(this, this)
        createRecognizer(preferOnDevice = false)
        realtimeCapture = RealtimeVoiceCapture(this, vadBenchmark::processPcm16)
        updateAudioRoute()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // A restarted process no longer owns the native ACP session. Do not show a zombie call.
        if (intent == null && !sessionActive.get()) {
            Log.w(TAG, "voice service restart rejected because the ACP session no longer exists")
            stopSelf(startId)
            return START_NOT_STICKY
        }
        when (intent?.action ?: ACTION_START) {
            ACTION_STOP -> {
                Log.i(TAG, "stop requested")
                stopConversation()
                return START_NOT_STICKY
            }
            ACTION_TOGGLE_MUTE -> {
                Log.i(TAG, "mute toggled")
                muted = !muted
                if (muted) stopListening() else scheduleListening(150)
                callActivity.get()?.updateMuteState(muted)
            }
            ACTION_PAUSE -> enterWakeOnlyMode()
            ACTION_RESUME -> resumeActiveMode()
            ACTION_TOGGLE_SPEAKER -> {
                speakerEnabled = !speakerEnabled
                getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
                    .edit().putBoolean("speaker", speakerEnabled).apply()
                updateAudioRoute()
            }
            ACTION_SET_LANGUAGES -> {
                languages = intent?.getStringArrayListExtra(EXTRA_LANGUAGES)
                    ?.filterTo(linkedSetOf()) { it == "zh-TW" || it == "en-US" }
                    ?.takeIf { it.isNotEmpty() }
                    ?: linkedSetOf("zh-TW", "en-US")
                getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
                    .edit().putStringSet("languages", languages).apply()
                stopListening()
                scheduleListening(200)
            }
            ACTION_SET_AUDIO_OUTPUT -> {
                speakerEnabled = intent?.getBooleanExtra(EXTRA_SPEAKER, true) ?: true
                getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
                    .edit().putBoolean("speaker", speakerEnabled).apply()
                updateAudioRoute()
            }
            ACTION_SET_SPEECH_OUTPUT -> {
                speechOutputEnabled = intent?.getBooleanExtra(EXTRA_SPEECH_OUTPUT, true) ?: true
                getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
                    .edit().putBoolean("speech_output", speechOutputEnabled).apply()
                if (!speechOutputEnabled) {
                    streamingHandler.removeCallbacks(flushStreamingSpeech)
                    streamingFlushScheduled = false
                    streamingBuffer.clear()
                    speechScheduler.interrupt()
                    tts?.stop()
                    orchestrator.playbackFinished()
                }
                publishState(if (waitingForAgent) "Waiting for Agent..." else currentIdleState())
            }
            ACTION_SUBMIT_TEXT -> {
                Log.i(TAG, "typed prompt received")
                latency.reset()
                latency.replace("speech_end")
                latency.mark("stt_commit")
                interruptOutputForNewPrompt()
                submitTranscript(intent?.getStringExtra(EXTRA_TEXT).orEmpty())
            }
            ACTION_AGENT_EVENT -> {
                val text = intent?.getStringExtra(EXTRA_TEXT).orEmpty()
                val kind = intent?.getStringExtra(EXTRA_EVENT_KIND).orEmpty()
                val epoch = intent?.getLongExtra(EXTRA_OUTPUT_EPOCH, -1L) ?: -1L
                if (kind.isNotBlank() && epoch == currentOutputEpoch()) {
                    outputWorker.execute {
                        val spokenText = traditionalChinese(text, outputSimplifiedToTraditional)
                        streamingHandler.post {
                            if (active && epoch == currentOutputEpoch()) {
                                acceptAgentEvent(kind, spokenText)
                            }
                        }
                    }
                }
            }
            ACTION_INTERRUPT_SPEECH -> {
                interruptSpeechOutput()
                publishState(currentIdleState())
                if (!usesRealtimeCapture) scheduleListening(150)
            }
        }
        if (intent?.action == ACTION_START || intent == null) {
            sessionActive.set(true)
            acquireCallResources()
        }
        ensureForeground()
        if (intent?.action in setOf(
                null,
                ACTION_START,
                ACTION_TOGGLE_MUTE,
                ACTION_TOGGLE_SPEAKER,
                ACTION_SET_LANGUAGES,
                ACTION_SET_AUDIO_OUTPUT,
                ACTION_SET_SPEECH_OUTPUT,
                ACTION_PAUSE,
                ACTION_RESUME,
            )
        ) {
            publishState(currentIdleState())
        }
        if (shouldListen() && !waitingForAgent && intent?.action in setOf(null, ACTION_START)) {
            scheduleListening(100)
        }
        return START_STICKY
    }

    override fun onDestroy() {
        Log.i(TAG, "voice service destroyed")
        active = false
        recognizer?.destroy()
        stopRealtimeCapture()
        streamingHandler.removeCallbacksAndMessages(null)
        inputWorker.shutdownNow()
        outputWorker.shutdownNow()
        tts?.shutdown()
        releaseCallResources()
        sessionActive.set(false)
        (getSystemService(AUDIO_SERVICE) as AudioManager).mode = AudioManager.MODE_NORMAL
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onInit(status: Int) {
        ttsReady = status == TextToSpeech.SUCCESS
        Log.i(TAG, "TTS initialized status=$status ready=$ttsReady engine=${tts?.defaultEngine}")
        if (ttsReady) {
            tts?.language = Locale.getDefault()
            tts?.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                override fun onStart(utteranceId: String?) {
                    latency.mark("audio_start")
                    latency.log(
                        "first-audio",
                        "after-agent-text" to "agent_first_text->audio_start",
                        "total" to "agent_request->audio_start",
                    )
                }
                override fun onAudioAvailable(utteranceId: String?, audio: ByteArray?) {
                    latency.mark("tts_first_pcm")
                    latency.log(
                        "tts-pipeline",
                        "text-to-pcm" to "tts_request->tts_first_pcm",
                        "pcm-to-audio" to "tts_first_pcm->audio_start",
                    )
                }
                override fun onRangeStart(utteranceId: String?, start: Int, end: Int, frame: Int) {
                    utteranceId?.let { speechScheduler.markRange(it, end) }
                }
                override fun onError(utteranceId: String?) {
                    if (!isSoftInterruptionActive()) {
                        val failed = utteranceId?.let(speechScheduler::fail)
                        if (failed != null) {
                            orchestrator.playbackFinished()
                            pumpSpeech()
                        }
                    }
                }
                override fun onDone(utteranceId: String?) {
                    val completed = utteranceId?.let(speechScheduler::complete)
                    if (completed != null) {
                        orchestrator.playbackFinished()
                        pumpSpeech()
                    }
                }
            })
            pumpSpeech()
        }
    }

    private fun startListening() {
        if (!shouldListen() || isListening) return
        val wakeOnly = voiceMode == VoiceMode.WAKE_ONLY
        val canTryRealtime = !wakeOnly && Build.VERSION.SDK_INT >= 33 && !realtimeCaptureDisabledForSession
        if (!canTryRealtime && (waitingForAgent || tts?.isSpeaking == true)) return
        if (!SpeechRecognizer.isRecognitionAvailable(this)) {
            Log.e(TAG, "speech recognition unavailable")
            publishState("Speech recognition is unavailable")
            return
        }
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            val selected = languages.toList()
            val primaryLanguage = if ("zh-TW" in selected) "zh-TW" else selected.firstOrNull() ?: "en-US"
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, primaryLanguage)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_PREFERENCE, primaryLanguage)
            if (Build.VERSION.SDK_INT >= 34 && selected.size > 1) {
                putExtra(RecognizerIntent.EXTRA_ENABLE_LANGUAGE_DETECTION, true)
                putExtra(RecognizerIntent.EXTRA_ENABLE_LANGUAGE_SWITCH, RecognizerIntent.LANGUAGE_SWITCH_BALANCED)
                putStringArrayListExtra(RecognizerIntent.EXTRA_LANGUAGE_DETECTION_ALLOWED_LANGUAGES, ArrayList(selected))
                putStringArrayListExtra(RecognizerIntent.EXTRA_LANGUAGE_SWITCH_ALLOWED_LANGUAGES, ArrayList(selected))
            }
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, wakeOnly)
            if (canTryRealtime) {
                runCatching {
                    val source = realtimeCapture?.start()
                        ?: error("Realtime audio capture is unavailable")
                    recognizerAudioSource = source
                    usesRealtimeCapture = true
                    putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, source)
                    putExtra(
                        RecognizerIntent.EXTRA_SEGMENTED_SESSION,
                        RecognizerIntent.EXTRA_AUDIO_SOURCE,
                    )
                    putExtra(
                        RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT,
                        RealtimeVoiceCapture.CHANNEL_COUNT,
                    )
                    putExtra(
                        RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING,
                        RealtimeVoiceCapture.ENCODING,
                    )
                    putExtra(
                        RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE,
                        RealtimeVoiceCapture.SAMPLE_RATE,
                    )
                }.onFailure {
                    Log.w(TAG, "realtime capture setup failed; using recognizer microphone", it)
                    stopRealtimeCapture()
                }
            }
        }
        Log.i(TAG, "starting recognizer realtime=$usesRealtimeCapture languages=$languages")
        publishState(if (wakeOnly) "Voice paused - say 開始通話" else "Listening...")
        isListening = true
        runCatching { recognizer?.startListening(intent) }
            .onFailure {
                Log.e(TAG, "startListening failed", it)
                isListening = false
                scheduleListening(1_200)
            }
    }

    private fun createRecognizer(preferOnDevice: Boolean) {
        recognizer?.destroy()
        wakeRecognizerIsOnDevice = preferOnDevice &&
            Build.VERSION.SDK_INT >= 31 &&
            SpeechRecognizer.isOnDeviceRecognitionAvailable(this)
        recognizer = if (wakeRecognizerIsOnDevice && Build.VERSION.SDK_INT >= 31) {
            runCatching { SpeechRecognizer.createOnDeviceSpeechRecognizer(this) }
                .onFailure { Log.w(TAG, "on-device recognizer unavailable; using system recognizer", it) }
                .getOrElse {
                    wakeRecognizerIsOnDevice = false
                    SpeechRecognizer.createSpeechRecognizer(this)
                }
        } else {
            SpeechRecognizer.createSpeechRecognizer(this)
        }.also { it.setRecognitionListener(this) }
        Log.i(TAG, "recognizer created on_device=$wakeRecognizerIsOnDevice")
    }

    private fun stopListening() {
        streamingHandler.removeCallbacks(restartListening)
        streamingHandler.removeCallbacks(finalizePartialTurn)
        if (isListening) recognizer?.cancel()
        isListening = false
        stopRealtimeCapture()
    }

    private fun stopRealtimeCapture() {
        val vadStats = vadBenchmark.snapshot()
        if (vadStats.frames > 0) Log.i(TAG, "vad-benchmark session=$vadStats")
        realtimeCapture?.stop()
        runCatching { recognizerAudioSource?.close() }
        recognizerAudioSource = null
        usesRealtimeCapture = false
        realtimeSessionReady = false
        vadBenchmark.reset()
    }

    private fun scheduleListening(delayMs: Long) {
        streamingHandler.removeCallbacks(restartListening)
        streamingHandler.postDelayed(restartListening, delayMs)
    }

    private fun shouldListen(): Boolean = active && !muted && voiceMode != VoiceMode.ENDED

    private fun currentIdleState(): String = when {
        voiceMode == VoiceMode.WAKE_ONLY -> "Voice paused - say 開始通話"
        waitingForAgent -> "Waiting for Agent..."
        muted -> "Muted"
        else -> "Listening..."
    }

    private fun enterWakeOnlyMode() {
        if (voiceMode != VoiceMode.ACTIVE) return
        Log.i(TAG, "entering wake-only voice mode")
        voiceMode = VoiceMode.WAKE_ONLY
        waitingForAgent = false
        invalidateOutput()
        NativeBridge.nativeInterruptVoiceAgent()
        orchestrator.cancelCurrentTurn()
        interruptSpeechOutput()
        stopListening()
        createRecognizer(preferOnDevice = true)
        releaseAudioFocusOnly()
        ZdroidSessionLocks.release(LOCK_OWNER)
        callActivity.get()?.updatePauseState(true)
        publishState(currentIdleState())
        scheduleListening(250)
    }

    private fun resumeActiveMode() {
        if (voiceMode != VoiceMode.WAKE_ONLY) return
        Log.i(TAG, "resuming active voice mode")
        stopListening()
        createRecognizer(preferOnDevice = false)
        voiceMode = VoiceMode.ACTIVE
        ZdroidSessionLocks.acquire(this, LOCK_OWNER)
        acquireAudioFocusOnly()
        callActivity.get()?.updatePauseState(false)
        publishState("Listening...")
        scheduleListening(150)
    }

    private fun handleVoiceModeCommand(text: String): Boolean {
        val normalized = text.lowercase(Locale.ROOT).replace(Regex("[\\p{P}\\p{S}\\s]+"), "")
        if (voiceMode == VoiceMode.WAKE_ONLY) {
            if (normalized in RESUME_VOICE_COMMANDS) {
                resumeActiveMode()
            } else {
                publishState(currentIdleState())
                scheduleListening(350)
            }
            return true
        }
        if (normalized in PAUSE_VOICE_COMMANDS) {
            enterWakeOnlyMode()
            return true
        }
        return false
    }

    private fun traditionalChinese(text: String, transliterator: Transliterator?): String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q &&
            "zh-TW" in languages && text.any { it.code in 0x3400..0x9FFF }
        ) {
            transliterator?.transliterate(text) ?: text
        } else {
            text
        }

    private fun acceptAgentEvent(kind: String, text: String) {
        val token = orchestrator.currentToken() ?: return
        val event = when (kind) {
            "message" -> VoiceOrchestrator.AgentEvent.Message(token, text)
            "thinking" -> VoiceOrchestrator.AgentEvent.Thinking(token)
            "tool_started" -> VoiceOrchestrator.AgentEvent.ToolStarted(token, text)
            "tool_finished" -> VoiceOrchestrator.AgentEvent.ToolFinished(token, text)
            "waiting_for_user" -> VoiceOrchestrator.AgentEvent.WaitingForUser(token, text)
            "status" -> VoiceOrchestrator.AgentEvent.Status(token, text)
            "finished" -> VoiceOrchestrator.AgentEvent.Finished(token)
            "failed" -> VoiceOrchestrator.AgentEvent.Failed(token, text)
            else -> return
        }
        if (!orchestrator.accept(event)) {
            Log.i(TAG, "dropping stale Agent event kind=$kind token=$token")
            return
        }
        waitingForAgent = when (event) {
            is VoiceOrchestrator.AgentEvent.Thinking,
            is VoiceOrchestrator.AgentEvent.ToolStarted,
            is VoiceOrchestrator.AgentEvent.ToolFinished -> true
            else -> false
        }
        if (event is VoiceOrchestrator.AgentEvent.Message) {
            acceptStreamingMessage(event)
        } else {
            val policyText = VoiceSpeechPolicy.prepare(event)
            if (policyText.isNotBlank()) {
                publishResponse(policyText)
                if (speechOutputEnabled) {
                    enqueueSpeech(policyText, VoiceSpeechPolicy.priority(event), token)
                }
            }
            if (event is VoiceOrchestrator.AgentEvent.Finished || event is VoiceOrchestrator.AgentEvent.Failed) {
                drainStreamingBuffer(force = true)
            }
            publishState(
                when (event) {
                    is VoiceOrchestrator.AgentEvent.Thinking -> "Agent is thinking..."
                    is VoiceOrchestrator.AgentEvent.ToolStarted -> "Agent is using ${event.label}..."
                    is VoiceOrchestrator.AgentEvent.WaitingForUser -> "Waiting for your answer"
                    is VoiceOrchestrator.AgentEvent.Failed -> "Agent stopped"
                    is VoiceOrchestrator.AgentEvent.Finished -> if (speechScheduler.hasPending()) "Agent speaking..." else "Listening..."
                    else -> "Agent is working..."
                },
            )
        }
    }

    private fun acceptStreamingMessage(event: VoiceOrchestrator.AgentEvent.Message) {
        val policyText = VoiceSpeechPolicy.prepare(event)
        if (policyText.isBlank()) return
        val startedSpeaking = lastStreamingResponse.isEmpty()
        waitingForAgent = false
        publishResponse(policyText)
        speechScheduler.updateGenerated(policyText)
        if (startedSpeaking) {
            latency.mark("agent_first_text")
            latency.log(
                "first-agent-text",
                "after-submit" to "agent_request->agent_first_text",
                "from-speech-end" to "speech_end->agent_first_text",
            )
            publishState(if (speechOutputEnabled) "Agent speaking..." else "Agent responding...")
        }
        val delta = when {
            policyText.startsWith(lastStreamingResponse) -> policyText.substring(lastStreamingResponse.length)
            else -> {
                speechScheduler.interrupt()
                tts?.stop()
                orchestrator.playbackFinished()
                streamingBuffer.clear()
                policyText
            }
        }
        lastStreamingResponse = policyText
        if (speechOutputEnabled) {
            if (delta.isNotEmpty()) streamingBuffer.append(delta)
            drainStreamingBuffer(force = delta.isEmpty())
            if (streamingBuffer.isNotEmpty()) scheduleStreamingFlush()
        }
    }

    private fun scheduleStreamingFlush() {
        if (streamingFlushScheduled) return
        streamingFlushScheduled = true
        streamingHandler.postDelayed(flushStreamingSpeech, STREAMING_TTS_FLUSH_MS)
    }

    private fun drainStreamingBuffer(force: Boolean) {
        while (streamingBuffer.isNotEmpty()) {
            val boundary = streamingBuffer.indexOfFirst { it in charArrayOf('。', '！', '？', '.', '!', '?', '\n') }
            val end = when {
                boundary >= 0 -> boundary + 1
                streamingBuffer.length >= 140 -> 140
                force -> streamingBuffer.length
                else -> return
            }
            val phrase = streamingBuffer.substring(0, end).trim()
            streamingBuffer.delete(0, end)
            if (phrase.isNotEmpty()) {
                orchestrator.currentToken()?.let { token ->
                    enqueueSpeech(phrase, VoiceSpeechScheduler.Priority.NORMAL, token)
                }
            }
        }
    }

    private fun enqueueSpeech(
        text: String,
        priority: VoiceSpeechScheduler.Priority,
        token: VoiceTurnManager.Token,
    ) {
        if (!speechOutputEnabled) return
        val preempt = speechScheduler.shouldPreempt(priority)
        if (preempt) {
            speechScheduler.pause()
            orchestrator.playbackPaused()
            tts?.stop()
        }
        val max = (TextToSpeech.getMaxSpeechInputLength() - 100).coerceAtLeast(500)
        text.chunked(max).forEach { speechScheduler.enqueue(token, it, priority) }
        if (preempt) streamingHandler.postDelayed({ pumpSpeech() }, 50L) else pumpSpeech()
    }

    private fun pumpSpeech() {
        if (!speechOutputEnabled || !ttsReady || isSoftInterruptionActive() || tts?.isSpeaking == true) return
        val speech = speechScheduler.next(orchestrator::accepts) ?: run {
            orchestrator.playbackFinished()
            resumeListeningAfterSpeech()
            return
        }
        if (!orchestrator.playbackStarted(speech.token)) return
        val locale = if (speech.text.any { it.code in 0x3400..0x9FFF } && "zh-TW" in languages) {
            Locale.TAIWAN
        } else {
            Locale.US
        }
        tts?.language = locale
        latency.mark("tts_request")
        tts?.speak(speech.text, TextToSpeech.QUEUE_FLUSH, null, speech.id)
    }

    private fun resumeListeningAfterSpeech() {
        streamingHandler.postDelayed({
            if (shouldListen() && voiceMode == VoiceMode.ACTIVE && tts?.isSpeaking != true && streamingBuffer.isEmpty()) {
                if (isSoftInterruptionActive()) return@postDelayed
                publishState("Listening...")
                if (!usesRealtimeCapture) scheduleListening(0)
            }
        }, 350)
    }

    private fun ensureForeground() {
        if (foregroundStarted) return
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            buildNotification(),
            if (Build.VERSION.SDK_INT >= 30) ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE else 0,
        )
        foregroundStarted = true
    }

    private fun acquireCallResources() {
        ZdroidSessionLocks.acquire(this, LOCK_OWNER)
        acquireAudioFocusOnly()
    }

    private fun acquireAudioFocusOnly() {
        val audioManager = getSystemService(AUDIO_SERVICE) as AudioManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            if (audioFocusRequest == null) {
                audioFocusRequest = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(
                        AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                            .build(),
                    )
                    .setAcceptsDelayedFocusGain(true)
                    .setOnAudioFocusChangeListener { focus ->
                        Log.i(TAG, "audio focus changed=$focus")
                    }
                    .build()
            }
            audioFocusRequest?.let(audioManager::requestAudioFocus)
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                null,
                AudioManager.STREAM_VOICE_CALL,
                AudioManager.AUDIOFOCUS_GAIN,
            )
        }
    }

    private fun releaseCallResources() {
        ZdroidSessionLocks.release(LOCK_OWNER)
        releaseAudioFocusOnly()
    }

    private fun releaseAudioFocusOnly() {
        val audioManager = getSystemService(AUDIO_SERVICE) as AudioManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            audioFocusRequest?.let(audioManager::abandonAudioFocusRequest)
            audioFocusRequest = null
        } else {
            @Suppress("DEPRECATION")
            audioManager.abandonAudioFocus(null)
        }
    }

    private fun submitTranscript(text: String) {
        val candidate = text.trim()
        if (candidate.isBlank()) return
        inputWorker.execute {
            val normalizedText = traditionalChinese(candidate, inputSimplifiedToTraditional)
            streamingHandler.post {
                if (active) submitNormalizedTranscript(normalizedText)
            }
        }
    }

    private fun submitRecognizedTranscript(text: String, confidence: Float?) {
        val candidate = text.trim()
        if (handleVoiceModeCommand(candidate)) return
        val speechDuration = currentSpeechDuration()
        if (!isMeaningfulSpeech(candidate, speechDuration, confidence, finalResult = true)) {
            Log.i(
                TAG,
                "discarding short/noisy speech duration=${speechDuration}ms confidence=$confidence text=${candidate.take(24)}",
            )
            latestPartialTranscript = ""
            speechStartedAt = 0L
            resumeSoftInterruptedOutput()
            publishTranscript("")
            publishState(
                when {
                    tts?.isSpeaking == true -> "Agent speaking..."
                    waitingForAgent -> "Waiting for Agent..."
                    muted -> "Muted"
                    else -> currentIdleState()
                },
            )
            return
        }
        confirmBargeIn(candidate, speechDuration)
        latency.mark("stt_commit")
        submitTranscript(candidate)
    }

    /**
     * A recognizer's beginning-of-speech callback is only a candidate signal. It may be a tap,
     * cough, speaker echo, or a filler sound, so it must never cancel an Agent response by itself.
     */
    private fun confirmBargeIn(transcript: String, speechDuration: Long) {
        if (isBargeInConfirmed()) return
        if (!isMeaningfulSpeech(transcript, speechDuration, confidence = null, finalResult = false)) return
        val actions = orchestrator.confirmInterruption()
        if (VoiceConversationPolicy.Action.CANCEL_AGENT !in actions) return
        latency.mark("interruption_detected")
        if (waitingForAgent || tts?.isSpeaking == true || streamingBuffer.isNotEmpty() || lastStreamingResponse.isNotEmpty()) {
            Log.i(TAG, "confirmed voice barge-in duration=${speechDuration}ms")
            invalidateOutput()
            interruptSpeechOutput()
            NativeBridge.nativeInterruptVoiceAgent()
            waitingForAgent = false
            orchestrator.cancelCurrentTurn()
            latency.mark("playback_stopped")
            logOutputProgress("interrupted")
            latency.log(
                "barge-in",
                "semantic-confirm" to "interruption_onset->interruption_detected",
                "audio-stop" to "interruption_onset->playback_stopped",
            )
        }
    }

    private fun hasAgentOutput(): Boolean =
        waitingForAgent || tts?.isSpeaking == true || speechScheduler.hasPending() ||
            streamingBuffer.isNotEmpty() || lastStreamingResponse.isNotEmpty()

    private fun isSoftInterruptionActive(): Boolean =
        orchestrator.snapshot().interruption == VoiceInterruptionArbiter.State.CANDIDATE

    private fun isBargeInConfirmed(): Boolean =
        orchestrator.snapshot().interruption == VoiceInterruptionArbiter.State.CONFIRMED

    /** Acoustic stage: stop audible output quickly, but retain it until STT confirms intent. */
    private fun beginSoftInterruption(
        source: String,
        elapsedMs: Long? = null,
        rmsDb: Double? = null,
        thresholdDb: Double? = null,
    ) {
        val actions = orchestrator.beginInterruption(hasAgentOutput())
        if (VoiceConversationPolicy.Action.PAUSE_PLAYBACK !in actions) return
        latency.mark(
            "interruption_onset",
            SystemClock.elapsedRealtime() - (elapsedMs ?: 0L),
        )
        speechScheduler.pause()
        orchestrator.playbackPaused()
        tts?.stop()
        streamingHandler.removeCallbacks(resumeAfterFalseAlarm)
        streamingHandler.postDelayed(resumeAfterFalseAlarm, SOFT_INTERRUPTION_TIMEOUT_MS)
        Log.i(
            TAG,
            "soft barge-in source=$source vad=${elapsedMs ?: -1}ms " +
                "rms=${rmsDb ?: Double.NaN} threshold=${thresholdDb ?: Double.NaN} " +
                "benchmark=${vadBenchmark.snapshot()}",
        )
        publishState("Listening (Agent paused)...")
    }

    private fun resumeSoftInterruptedOutput() {
        if (!isSoftInterruptionActive()) return
        streamingHandler.removeCallbacks(resumeAfterFalseAlarm)
        val actions = orchestrator.recoverFalseInterruption()
        if (VoiceConversationPolicy.Action.RESUME_PLAYBACK !in actions) return
        orchestrator.playbackResumed()
        Log.i(TAG, "resuming after false barge-in")
        pumpSpeech()
        publishState(
            when {
                speechScheduler.hasPending() -> "Agent speaking..."
                waitingForAgent -> "Waiting for Agent..."
                else -> "Listening..."
            },
        )
    }

    private fun logOutputProgress(reason: String) {
        val snapshot = speechScheduler.snapshot()
        Log.i(
            TAG,
            "voice-output $reason generated=${snapshot.generatedText.length} " +
                "queued=${snapshot.queuedText.length} spoken=${snapshot.spokenText.length} " +
                "cursor=${snapshot.spokenCursor}",
        )
    }

    private fun currentSpeechDuration(): Long =
        if (speechStartedAt == 0L) 0L else (SystemClock.elapsedRealtime() - speechStartedAt).coerceAtLeast(0L)

    private fun isMeaningfulSpeech(
        text: String,
        durationMs: Long,
        confidence: Float?,
        finalResult: Boolean,
    ): Boolean {
        val normalized = text
            .lowercase(Locale.ROOT)
            .replace(Regex("[\\p{P}\\p{S}\\s]+"), "")
        if (normalized.isBlank() || normalized.matches(FILLER_ONLY_REGEX)) return false

        val immediateCommand = IMMEDIATE_INTERRUPT_COMMANDS.any { normalized == it }
        if (immediateCommand) return true
        if (confidence != null && confidence >= 0f && confidence < MIN_RECOGNITION_CONFIDENCE) {
            return false
        }

        val contentLength = normalized.codePointCount(0, normalized.length)
        return if (finalResult) {
            durationMs >= MIN_FINAL_SPEECH_MS || contentLength >= MIN_FINAL_CONTENT_LENGTH
        } else {
            durationMs >= MIN_BARGE_IN_SPEECH_MS && contentLength >= MIN_PARTIAL_CONTENT_LENGTH
        }
    }

    private fun submitNormalizedTranscript(normalizedText: String) {
        if (normalizedText.isBlank()) return
        val reflex = PhoneControlSession.resolveAndExecute(this, normalizedText)
        if (reflex.handled) {
            lastSubmittedTranscript = normalizedText
            speechStartedAt = 0L
            latestPartialTranscript = ""
            publishTranscript(normalizedText)
            publishResponse(reflex.message)
            publishState("Phone action completed")
            if (!usesRealtimeCapture) scheduleListening(200)
            return
        }
        if (normalizedText == lastSubmittedTranscript && waitingForAgent) return
        lastSubmittedTranscript = normalizedText
        if (!isBargeInConfirmed() &&
            (waitingForAgent || tts?.isSpeaking == true || streamingBuffer.isNotEmpty() || lastStreamingResponse.isNotEmpty())
        ) {
            interruptOutputForNewPrompt()
        }
        speechStartedAt = 0L
        latestPartialTranscript = ""
        streamingHandler.removeCallbacks(finalizePartialTurn)
        Log.i(TAG, "submitting transcript (${normalizedText.length} chars)")
        if (!usesRealtimeCapture) stopListening()
        lastStreamingResponse = ""
        streamingBuffer.clear()
        waitingForAgent = true
        speechScheduler.resetTurn()
        latency.mark("agent_request")
        orchestrator.commitTurn(normalizedText)
        publishTranscript(normalizedText)
        NativeBridge.nativeImeCommitText(0L, normalizedText, 1)
        val meta = android.view.KeyEvent.META_CTRL_ON or android.view.KeyEvent.META_SHIFT_ON
        NativeBridge.nativeImeSendKeyEvent(0L, android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.KEYCODE_ENTER, meta, 0)
        NativeBridge.nativeImeSendKeyEvent(0L, android.view.KeyEvent.ACTION_UP, android.view.KeyEvent.KEYCODE_ENTER, meta, 0)
        publishState("Waiting for Agent...")
    }

    private fun interruptOutputForNewPrompt() {
        invalidateOutput()
        if (!usesRealtimeCapture) stopListening()
        orchestrator.cancelCurrentTurn()
        interruptSpeechOutput()
        NativeBridge.nativeInterruptVoiceAgent()
        waitingForAgent = false
        publishState("Sending message...")
    }

    private fun interruptSpeechOutput() {
        tts?.stop()
        streamingHandler.removeCallbacks(flushStreamingSpeech)
        streamingFlushScheduled = false
        streamingBuffer.clear()
        lastStreamingResponse = ""
        speechScheduler.interrupt()
        orchestrator.playbackFinished()
        streamingHandler.removeCallbacks(resumeAfterFalseAlarm)
    }

    private fun stopConversation() {
        voiceMode = VoiceMode.ENDED
        stopListening()
        tts?.stop()
        streamingHandler.removeCallbacksAndMessages(null)
        streamingBuffer.clear()
        lastStreamingResponse = ""
        speechScheduler.resetTurn()
        orchestrator.reset()
        endpointing.reset()
        sessionActive.set(false)
        releaseCallResources()
        mainActivity.get()?.onNativeVoiceConversationEnded()
        callActivity.get()?.finishCall()
        getSharedPreferences("zdroid_voice", Context.MODE_PRIVATE)
            .edit().remove("call_started_at").apply()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun updateAudioRoute() {
        val audio = getSystemService(AUDIO_SERVICE) as AudioManager
        audio.mode = AudioManager.MODE_IN_COMMUNICATION
        @Suppress("DEPRECATION")
        audio.isSpeakerphoneOn = speakerEnabled
    }

    private fun buildNotification(): android.app.Notification {
        val endCallIntent = serviceIntent(ACTION_STOP, 3)
        val resumeTitle = SpannableString("▶ Resume").apply {
            setSpan(
                ForegroundColorSpan(android.graphics.Color.rgb(50, 205, 112)),
                0,
                length,
                Spanned.SPAN_EXCLUSIVE_EXCLUSIVE,
            )
            setSpan(StyleSpan(android.graphics.Typeface.BOLD), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
        val pauseResumeAction = NotificationCompat.Action.Builder(
            if (voiceMode == VoiceMode.WAKE_ONLY) R.drawable.ic_voice_play else R.drawable.ic_voice_pause,
            if (voiceMode == VoiceMode.WAKE_ONLY) resumeTitle else "Pause",
            serviceIntent(if (voiceMode == VoiceMode.WAKE_ONLY) ACTION_RESUME else ACTION_PAUSE, 4),
        ).build()
        val agent = Person.Builder()
            .setName("Zdroid-B Agent")
            .setImportant(true)
            .build()
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.sym_action_call)
            .setContentTitle("Zdroid-B live audio chat")
            .setContentText(
                when {
                    voiceMode == VoiceMode.WAKE_ONLY -> "Paused. Say 開始通話 to resume"
                    waitingForAgent -> "Agent is working. Live audio chat is in progress"
                    muted -> "Microphone muted. Live audio chat is in progress"
                    !speechOutputEnabled -> "Listening with voice output off"
                    else -> "Listening. Live audio chat is in progress"
                },
            )
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setWhen(callStartedAt)
            .setUsesChronometer(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setColor(
                if (voiceMode == VoiceMode.WAKE_ONLY) android.graphics.Color.rgb(50, 205, 112)
                else android.graphics.Color.rgb(116, 204, 168),
            )
            .setContentIntent(activityIntent())
            .setStyle(NotificationCompat.CallStyle.forOngoingCall(agent, endCallIntent))
            // CallStyle permits at most two custom actions. Keep the controls
            // that change conversation state visible; audio routing remains in
            // the call UI settings.
            .addAction(pauseResumeAction)
            .addAction(0, if (muted) "Unmute" else "Mute", serviceIntent(ACTION_TOGGLE_MUTE, 1))
            .build()
    }

    private fun activityIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, VoiceCallActivity::class.java)
            .putExtra(VoiceCallActivity.EXTRA_AGENT_NAME, "Agent")
            .putExtra(VoiceCallActivity.EXTRA_MODEL_NAME, "Current model")
            .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun serviceIntent(action: String, requestCode: Int): PendingIntent = PendingIntent.getService(
        this,
        requestCode,
        Intent(this, VoiceConversationService::class.java).setAction(action),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun publishState(value: String) {
        callActivity.get()?.updateState(value)
        val now = System.currentTimeMillis()
        if (value != lastPublishedState || now - lastNotificationAt >= NOTIFICATION_THROTTLE_MS) {
            lastPublishedState = value
            lastNotificationAt = now
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, buildNotification())
        }
    }
    private fun publishTranscript(value: String) = callActivity.get()?.updateTranscript(value)
    private fun publishResponse(value: String) = callActivity.get()?.updateResponse(value)

    override fun onReadyForSpeech(params: Bundle?) {
        Log.i(TAG, "recognizer ready realtime=$usesRealtimeCapture")
        isListening = true
        if (usesRealtimeCapture) realtimeSessionReady = true
        publishState(currentIdleState())
    }
    override fun onBeginningOfSpeech() {
        Log.i(TAG, "speech beginning")
        if (voiceMode == VoiceMode.WAKE_ONLY) {
            speechStartedAt = SystemClock.elapsedRealtime()
            return
        }
        if (!hasAgentOutput()) latency.reset()
        latency.mark("speech_start")
        speechStartedAt = SystemClock.elapsedRealtime()
        endpointing.speechResumed(speechStartedAt)
        beginSoftInterruption("recognizer")
        orchestrator.userSpeechStarted()
        publishState(if (isSoftInterruptionActive()) "Listening (Agent paused)..." else "Listening...")
    }
    override fun onRmsChanged(rmsdB: Float) = Unit
    override fun onBufferReceived(buffer: ByteArray?) = Unit
    override fun onEndOfSpeech() {
        Log.i(TAG, "speech ended partial=${latestPartialTranscript.length}")
        latency.replace("speech_end")
        endpointing.speechEnded(SystemClock.elapsedRealtime())
        publishState("Processing speech...")
        streamingHandler.removeCallbacks(finalizePartialTurn)
        streamingHandler.postDelayed(finalizePartialTurn, endpointing.timeoutMs())
    }
    override fun onEvent(eventType: Int, params: Bundle?) = Unit
    override fun onPartialResults(partialResults: Bundle?) {
        val partial = partialResults
            ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
            ?.firstOrNull()
            ?: return
        inputWorker.execute {
            val normalized = traditionalChinese(partial, inputSimplifiedToTraditional)
            streamingHandler.post {
                if (active && isListening) {
                    latestPartialTranscript = normalized
                    consecutiveRecognizerErrors = 0
                    if (voiceMode == VoiceMode.WAKE_ONLY) return@post
                    publishTranscript(normalized)
                    confirmBargeIn(normalized, currentSpeechDuration())
                    if (!hasAgentOutput() || isBargeInConfirmed()) {
                        orchestrator.updateProvisionalTranscript(normalized)
                    }
                    streamingHandler.removeCallbacks(finalizePartialTurn)
                    streamingHandler.postDelayed(finalizePartialTurn, endpointing.timeoutMs())
                }
            }
        }
    }
    override fun onResults(results: Bundle?) {
        Log.i(TAG, "recognizer final results")
        streamingHandler.removeCallbacks(finalizePartialTurn)
        if (!usesRealtimeCapture) isListening = false
        latency.replace("speech_end")
        val transcript = results
            ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
            ?.firstOrNull()
            .orEmpty()
        val confidence = results
            ?.getFloatArray(SpeechRecognizer.CONFIDENCE_SCORES)
            ?.firstOrNull()
        submitRecognizedTranscript(transcript, confidence)
    }
    override fun onSegmentResults(segmentResults: Bundle) {
        Log.i(TAG, "recognizer segment results")
        streamingHandler.removeCallbacks(finalizePartialTurn)
        latency.replace("speech_end")
        submitRecognizedTranscript(
            segmentResults
                .getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
                .orEmpty(),
            segmentResults.getFloatArray(SpeechRecognizer.CONFIDENCE_SCORES)?.firstOrNull(),
        )
    }
    override fun onEndOfSegmentedSession() {
        isListening = false
        stopRealtimeCapture()
        if (shouldListen()) scheduleListening(400)
    }
    override fun onError(error: Int) {
        Log.w(TAG, "recognizer error=$error realtime=$usesRealtimeCapture ready=$realtimeSessionReady")
        streamingHandler.removeCallbacks(finalizePartialTurn)
        isListening = false
        val wasRealtime = usesRealtimeCapture
        val realtimeStartedSuccessfully = realtimeSessionReady
        stopRealtimeCapture()
        if (wasRealtime && !realtimeStartedSuccessfully) {
            realtimeCaptureDisabledForSession = true
        }
        consecutiveRecognizerErrors += 1
        if (wasRealtime && consecutiveRecognizerErrors >= REALTIME_ERROR_LIMIT) {
            realtimeCaptureDisabledForSession = true
            Log.w(TAG, "disabling realtime audio source after $consecutiveRecognizerErrors errors")
        }
        if (shouldListen() && !waitingForAgent) {
            val baseDelay = when (error) {
                SpeechRecognizer.ERROR_RECOGNIZER_BUSY -> 1_500L
                SpeechRecognizer.ERROR_TOO_MANY_REQUESTS -> 3_000L
                else -> 900L
            }
            val delay = (baseDelay * consecutiveRecognizerErrors.coerceAtMost(5)).coerceAtMost(8_000L)
            if (wasRealtime && !realtimeStartedSuccessfully) {
                publishState("Using compatible voice recognition...")
            } else if (wasRealtime) {
                publishState("Restarting realtime listening...")
            }
            scheduleListening(delay)
        }
    }

    companion object {
        // v2 avoids inheriting the old LOW-importance channel after upgrade;
        // ongoing call presentation needs DEFAULT visibility on modern Android.
        private const val CHANNEL_ID = "zdroid_voice_conversation_v2"
        private const val NOTIFICATION_ID = 1450
        private const val ACTION_START = "com.zdroid.voice.START"
        private const val ACTION_STOP = "com.zdroid.voice.STOP"
        private const val ACTION_AGENT_EVENT = "com.zdroid.voice.AGENT_EVENT"
        private const val ACTION_TOGGLE_MUTE = "com.zdroid.voice.TOGGLE_MUTE"
        private const val ACTION_TOGGLE_SPEAKER = "com.zdroid.voice.TOGGLE_SPEAKER"
        private const val ACTION_SET_LANGUAGES = "com.zdroid.voice.SET_LANGUAGES"
        private const val ACTION_SET_AUDIO_OUTPUT = "com.zdroid.voice.SET_AUDIO_OUTPUT"
        private const val ACTION_SET_SPEECH_OUTPUT = "com.zdroid.voice.SET_SPEECH_OUTPUT"
        private const val ACTION_SUBMIT_TEXT = "com.zdroid.voice.SUBMIT_TEXT"
        private const val ACTION_INTERRUPT_SPEECH = "com.zdroid.voice.INTERRUPT_SPEECH"
        private const val ACTION_PAUSE = "com.zdroid.voice.PAUSE"
        private const val ACTION_RESUME = "com.zdroid.voice.RESUME"
        private const val EXTRA_TEXT = "text"
        private const val EXTRA_EVENT_KIND = "event_kind"
        private const val EXTRA_OUTPUT_EPOCH = "output_epoch"
        private const val EXTRA_LANGUAGES = "languages"
        private const val EXTRA_SPEAKER = "speaker"
        private const val EXTRA_SPEECH_OUTPUT = "speech_output"
        private const val TAG = "ZdroidVoice"
        private const val STREAMING_TTS_FLUSH_MS = 180L
        private const val SOFT_INTERRUPTION_TIMEOUT_MS = 900L
        private const val MIN_BARGE_IN_SPEECH_MS = 220L
        private const val MIN_FINAL_SPEECH_MS = 350L
        private const val MIN_PARTIAL_CONTENT_LENGTH = 2
        private const val MIN_FINAL_CONTENT_LENGTH = 3
        private const val MIN_RECOGNITION_CONFIDENCE = 0.35f
        private const val NOTIFICATION_THROTTLE_MS = 1_000L
        private const val REALTIME_ERROR_LIMIT = 3
        private const val LOCK_OWNER = "voice-conversation"
        private val FILLER_ONLY_REGEX = Regex(
            "(?:啊+|阿+|喔+|哦+|噢+|嗯+|呃+|欸+|誒+|唉+|哎+|哈+|呵+|诶+|额+|uh+|um+|h+m+|m+h+m+|ah+|oh+|er+|erm+)+",
            RegexOption.IGNORE_CASE,
        )
        private val IMMEDIATE_INTERRUPT_COMMANDS = setOf(
            "停",
            "停止",
            "停下",
            "等一下",
            "等等",
            "先停",
            "不要",
            "不對",
            "stop",
            "wait",
            "pause",
            "cancel",
            "holdon",
            "nevermind",
        )
        private val PAUSE_VOICE_COMMANDS = setOf(
            "暫停通話",
            "暫停對話",
            "pauseconversation",
            "pausevoice",
        )
        private val RESUME_VOICE_COMMANDS = setOf(
            "開始通話",
            "繼續通話",
            "恢復通話",
            "resumeconversation",
            "resumevoice",
        )
        private var mainActivity = WeakReference<MainActivity>(null)
        private var callActivity = WeakReference<VoiceCallActivity>(null)
        private val outputEpoch = AtomicLong(0L)
        private val sessionActive = java.util.concurrent.atomic.AtomicBoolean(false)

        fun attachMainActivity(value: MainActivity?) { mainActivity = WeakReference(value) }
        fun attachCallActivity(value: VoiceCallActivity?) { callActivity = WeakReference(value) }
        fun start(context: Context) {
            ensureChannel(context)
            ContextCompat.startForegroundService(context, Intent(context, VoiceConversationService::class.java).setAction(ACTION_START))
        }
        fun stop(context: Context) {
            context.startService(Intent(context, VoiceConversationService::class.java).setAction(ACTION_STOP))
        }
        fun currentOutputEpoch(): Long = outputEpoch.get()
        fun isSessionActive(): Boolean = sessionActive.get()
        fun invalidateOutput(): Long = outputEpoch.incrementAndGet()
        fun agentEvent(
            context: Context,
            kind: String,
            text: String,
            epoch: Long = currentOutputEpoch(),
        ) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, VoiceConversationService::class.java)
                    .setAction(ACTION_AGENT_EVENT)
                    .putExtra(EXTRA_EVENT_KIND, kind)
                    .putExtra(EXTRA_TEXT, text)
                    .putExtra(EXTRA_OUTPUT_EPOCH, epoch),
            )
        }
        fun submitTyped(context: Context, text: String) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, VoiceConversationService::class.java)
                    .setAction(ACTION_SUBMIT_TEXT)
                    .putExtra(EXTRA_TEXT, text),
            )
        }
        fun toggleMute(context: Context) {
            context.startService(
                Intent(context, VoiceConversationService::class.java).setAction(ACTION_TOGGLE_MUTE),
            )
        }
        fun pause(context: Context) {
            context.startService(Intent(context, VoiceConversationService::class.java).setAction(ACTION_PAUSE))
        }
        fun resume(context: Context) {
            context.startService(Intent(context, VoiceConversationService::class.java).setAction(ACTION_RESUME))
        }
        fun setLanguages(context: Context, languages: Set<String>) {
            context.startService(
                Intent(context, VoiceConversationService::class.java)
                    .setAction(ACTION_SET_LANGUAGES)
                    .putStringArrayListExtra(EXTRA_LANGUAGES, ArrayList(languages)),
            )
        }
        fun interruptSpeech(context: Context) {
            context.startService(
                Intent(context, VoiceConversationService::class.java).setAction(ACTION_INTERRUPT_SPEECH),
            )
        }
        fun setAudioOutput(context: Context, speaker: Boolean) {
            context.startService(
                Intent(context, VoiceConversationService::class.java)
                    .setAction(ACTION_SET_AUDIO_OUTPUT)
                    .putExtra(EXTRA_SPEAKER, speaker),
            )
        }
        fun setSpeechOutput(context: Context, enabled: Boolean) {
            context.startService(
                Intent(context, VoiceConversationService::class.java)
                    .setAction(ACTION_SET_SPEECH_OUTPUT)
                    .putExtra(EXTRA_SPEECH_OUTPUT, enabled),
            )
        }
        private fun ensureChannel(context: Context) {
            if (Build.VERSION.SDK_INT >= 26) {
                val channel = NotificationChannel(
                    CHANNEL_ID,
                    "Live audio conversations",
                    NotificationManager.IMPORTANCE_DEFAULT,
                ).apply {
                    description = "Ongoing hands-free Agent conversations"
                    setSound(null, null)
                    enableVibration(false)
                }
                context.getSystemService(NotificationManager::class.java)
                    .createNotificationChannel(channel)
            }
        }
    }
}
