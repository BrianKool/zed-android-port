package com.zdroid

import android.util.Log
import io.droidmcp.core.McpTool
import io.droidmcp.core.ToolRegistry
import io.droidmcp.core.protocol.McpProtocolImpl
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.runBlocking

/** Minimal MCP HTTP transport that is physically restricted to Android loopback. */
class LoopbackMcpServer(
    tools: List<McpTool>,
    private val port: Int,
    private val bearerToken: String,
) {
    private val registry = ToolRegistry().apply { registerAll(tools) }
    private val protocol = McpProtocolImpl(registry, serverName = "zdroid-phone-use")
    private val workers = ThreadPoolExecutor(
        WORKER_COUNT,
        WORKER_COUNT,
        0L,
        TimeUnit.MILLISECONDS,
        ArrayBlockingQueue(MAX_QUEUED_REQUESTS),
        { task -> Thread(task, "zdroid-mcp-worker").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    )
    private val running = AtomicBoolean(false)
    @Volatile private var socket: ServerSocket? = null
    @Volatile private var acceptThread: Thread? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        try {
            val server = ServerSocket(port, 32, InetAddress.getByName(LOOPBACK))
            socket = server
            acceptThread = Thread({ acceptLoop(server) }, "zdroid-mcp-loopback").apply {
                isDaemon = true
                start()
            }
        } catch (error: Throwable) {
            running.set(false)
            throw error
        }
    }

    fun stop() {
        running.set(false)
        runCatching { socket?.close() }
        socket = null
        acceptThread?.interrupt()
        acceptThread = null
        workers.shutdownNow()
    }

    fun isRunning(): Boolean = running.get() && socket?.isClosed == false

    private fun acceptLoop(server: ServerSocket) {
        while (running.get()) {
            val client = try {
                server.accept()
            } catch (error: Throwable) {
                if (running.get()) Log.w(TAG, "MCP loopback accept failed", error)
                break
            }
            try {
                workers.execute {
                    client.use { connection ->
                        runCatching { handle(connection) }
                            .onFailure { Log.w(TAG, "MCP loopback request failed", it) }
                    }
                }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
                client.close()
            }
        }
    }

    private fun handle(client: Socket) {
        client.soTimeout = REQUEST_TIMEOUT_MS
        val input = BufferedInputStream(client.getInputStream())
        val output = BufferedOutputStream(client.getOutputStream())
        val requestLine = readLine(input, MAX_HEADER_LINE) ?: return
        val parts = requestLine.split(' ')
        if (parts.size < 2) return respond(output, 400, "Bad Request", "")
        val method = parts[0]
        val path = parts[1]
        val headers = linkedMapOf<String, String>()
        var headerCount = 0
        while (headerCount < MAX_HEADERS) {
            val line = readLine(input, MAX_HEADER_LINE) ?: return
            if (line.isEmpty()) break
            val separator = line.indexOf(':')
            if (separator > 0) {
                headers[line.substring(0, separator).trim().lowercase()] =
                    line.substring(separator + 1).trim()
            }
            headerCount += 1
        }
        if (headerCount >= MAX_HEADERS) return respond(output, 431, "Request Header Fields Too Large", "")
        val authorization = headers["authorization"].orEmpty()
        val supplied = authorization.removePrefix("Bearer ")
        if (!MessageDigest.isEqual(
                supplied.toByteArray(StandardCharsets.UTF_8),
                bearerToken.toByteArray(StandardCharsets.UTF_8),
            )
        ) {
            return respond(output, 401, "Unauthorized", "{\"error\":\"Invalid or missing token\"}")
        }
        if (path == "/health" && method == "GET") {
            return respond(output, 200, "OK", "{\"status\":\"ok\",\"tools\":${registry.listEnabledTools().size}}")
        }
        if (path != "/mcp") return respond(output, 404, "Not Found", "")
        if (method == "DELETE") return respond(output, 200, "OK", "{}")
        if (method != "POST") return respond(output, 405, "Method Not Allowed", "")
        val length = headers["content-length"]?.toIntOrNull()
            ?: return respond(output, 411, "Length Required", "")
        if (length !in 0..MAX_BODY_BYTES) {
            return respond(output, 413, "Payload Too Large", "")
        }
        val bodyBytes = ByteArray(length)
        var read = 0
        while (read < length) {
            val count = input.read(bodyBytes, read, length - read)
            if (count < 0) return respond(output, 400, "Bad Request", "")
            read += count
        }
        val body = String(bodyBytes, StandardCharsets.UTF_8)
        val response = runBlocking { protocol.handleMessage(body, clientLabel = "local") }
        val extraHeaders = if (INITIALIZE.containsMatchIn(body)) {
            mapOf("Mcp-Session-Id" to UUID.randomUUID().toString())
        } else emptyMap()
        if (response.isEmpty()) respond(output, 202, "Accepted", "", extraHeaders)
        else respond(output, 200, "OK", response, extraHeaders)
    }

    private fun respond(
        output: BufferedOutputStream,
        code: Int,
        reason: String,
        body: String,
        extraHeaders: Map<String, String> = emptyMap(),
    ) {
        val bytes = body.toByteArray(StandardCharsets.UTF_8)
        val headers = buildString {
            append("HTTP/1.1 $code $reason\r\n")
            append("Content-Type: application/json; charset=utf-8\r\n")
            append("Content-Length: ${bytes.size}\r\n")
            append("Connection: close\r\n")
            extraHeaders.forEach { (name, value) -> append("$name: $value\r\n") }
            append("\r\n")
        }
        output.write(headers.toByteArray(StandardCharsets.US_ASCII))
        output.write(bytes)
        output.flush()
    }

    private fun readLine(input: BufferedInputStream, limit: Int): String? {
        val bytes = ArrayList<Byte>()
        while (bytes.size < limit) {
            val value = input.read()
            if (value < 0) return if (bytes.isEmpty()) null else String(bytes.toByteArray())
            if (value == '\n'.code) break
            if (value != '\r'.code) bytes.add(value.toByte())
        }
        if (bytes.size >= limit) throw IllegalArgumentException("HTTP header line exceeds limit")
        return String(bytes.toByteArray(), StandardCharsets.US_ASCII)
    }

    companion object {
        private const val TAG = "LoopbackMcpServer"
        private const val LOOPBACK = "127.0.0.1"
        private const val MAX_HEADERS = 64
        private const val MAX_HEADER_LINE = 8 * 1024
        private const val MAX_BODY_BYTES = 1024 * 1024
        private const val REQUEST_TIMEOUT_MS = 15_000
        private const val WORKER_COUNT = 4
        private const val MAX_QUEUED_REQUESTS = 16
        private val INITIALIZE = Regex("\\\"method\\\"\\s*:\\s*\\\"initialize\\\"")
    }
}
