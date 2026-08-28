package com.zdroid

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.os.ParcelFileDescriptor
import androidx.core.content.ContextCompat
import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * Owns one continuous, voice-communication audio capture session.
 *
 * The read side of the pipe is consumed by Android's segmented speech recognizer while the
 * write side stays attached to AudioRecord. Keeping AudioRecord alive allows AEC/NS and barge-in
 * without repeatedly releasing and reacquiring the microphone.
 */
class RealtimeVoiceCapture(
    private val context: Context,
    private val onAudioFrame: ((ByteArray, Int) -> Unit)? = null,
) {
    private var recorder: AudioRecord? = null
    private var echoCanceler: AcousticEchoCanceler? = null
    private var noiseSuppressor: NoiseSuppressor? = null
    private var readPipe: ParcelFileDescriptor? = null
    private var writePipe: ParcelFileDescriptor? = null
    private var captureThread: Thread? = null
    private val running = AtomicBoolean(false)

    fun start(): ParcelFileDescriptor {
        check(!running.get()) { "Realtime voice capture is already running" }
        check(
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED,
        ) { "Microphone permission is required for realtime voice capture" }
        val minimum = AudioRecord.getMinBufferSize(SAMPLE_RATE, CHANNEL_CONFIG, ENCODING)
        check(minimum > 0) { "AudioRecord does not support 16 kHz mono PCM" }
        val audioRecord = AudioRecord.Builder()
            .setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(ENCODING)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(CHANNEL_CONFIG)
                    .build(),
            )
            .setBufferSizeInBytes(minimum * 2)
            .build()
        check(audioRecord.state == AudioRecord.STATE_INITIALIZED) { "AudioRecord failed to initialise" }

        val pipe = ParcelFileDescriptor.createPipe()
        recorder = audioRecord
        readPipe = pipe[0]
        writePipe = pipe[1]
        echoCanceler = AcousticEchoCanceler.create(audioRecord.audioSessionId)?.apply {
            runCatching { enabled = true }
        }
        noiseSuppressor = NoiseSuppressor.create(audioRecord.audioSessionId)?.apply {
            runCatching { enabled = true }
        }
        running.set(true)
        audioRecord.startRecording()
        val output = ParcelFileDescriptor.AutoCloseOutputStream(pipe[1])
        captureThread = thread(name = "ZdroidAudioCapture") {
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
            val buffer = ByteArray(minimum)
            try {
                while (running.get()) {
                    val count = audioRecord.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)
                    if (count > 0) {
                        onAudioFrame?.invoke(buffer, count)
                        output.write(buffer, 0, count)
                    }
                }
            } catch (_: IOException) {
                // Closing the pipe is the normal way to end a segmented recognition session.
            } finally {
                runCatching { output.close() }
            }
        }
        return pipe[0]
    }

    fun stop() {
        if (!running.getAndSet(false)) return
        runCatching { recorder?.stop() }
        runCatching { writePipe?.close() }
        captureThread?.interrupt()
        runCatching { captureThread?.join(750) }
        runCatching { readPipe?.close() }
        runCatching { echoCanceler?.release() }
        runCatching { noiseSuppressor?.release() }
        runCatching { recorder?.release() }
        captureThread = null
        writePipe = null
        readPipe = null
        echoCanceler = null
        noiseSuppressor = null
        recorder = null
    }

    companion object {
        const val SAMPLE_RATE = 16_000
        const val CHANNEL_COUNT = 1
        const val CHANNEL_CONFIG = AudioFormat.CHANNEL_IN_MONO
        const val ENCODING = AudioFormat.ENCODING_PCM_16BIT
    }
}
