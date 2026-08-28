package com.zdroid

import android.content.Context
import io.droidmcp.core.McpTool
import io.droidmcp.core.ParameterType
import io.droidmcp.core.ToolAnnotations
import io.droidmcp.core.ToolParameter
import io.droidmcp.core.ToolResult
import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/** A format-scoped bridge to the validated Office engine in Managed Linux. */
class OfficeDocumentTool(
    private val context: Context,
    private val plugin: OfficePlugin,
) : McpTool {
    override val name = "${plugin.id}_document"
    override val description = plugin.toolDescription
    override val parameters = listOf(
        ToolParameter("action", plugin.actions.joinToString(", "), ParameterType.STRING, true),
        ToolParameter("input_path", "Absolute input file path for inspect/extract/reconcile.", ParameterType.STRING, false),
        ToolParameter("second_input_path", "Second workbook for Excel reconciliation.", ParameterType.STRING, false),
        ToolParameter("output_path", "New output path. Existing source files are never overwritten.", ParameterType.STRING, false),
        ToolParameter("options_json", "JSON object containing format-specific options.", ParameterType.STRING, false),
    )
    override val annotations = ToolAnnotations(readOnlyHint = false, idempotentHint = false)

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        if (!OfficePluginManager.isInstalled(context, plugin)) {
            return ToolResult.error("plugin_not_installed", "${plugin.label} is not installed. Open Settings > Plugins.")
        }
        if (!OfficePluginManager.isActive(context, plugin)) {
            return ToolResult.error("plugin_inactive", "${plugin.label} is inactive. Open Settings > Plugins.")
        }
        val action = params["action"]?.toString()?.lowercase().orEmpty()
        if (action !in plugin.actions) {
            return ToolResult.error("unsupported_action", "${plugin.label} supports ${plugin.actions.joinToString()}")
        }
        val input = checkedPath(params["input_path"]?.toString(), output = false)
        val second = checkedPath(params["second_input_path"]?.toString(), output = false)
        val output = checkedPath(params["output_path"]?.toString(), output = true)
        val rawOptions = params["options_json"]?.toString() ?: "{}"
        if (rawOptions.length > OfficePluginManager.MAX_OPTIONS_CHARACTERS) {
            return ToolResult.error("invalid_options", "options_json is too large")
        }
        val options = runCatching { JSONObject(rawOptions) }
            .getOrElse { return ToolResult.error("invalid_options", "options_json must be a JSON object") }
        if (input is PathResult.Error) return ToolResult.error("invalid_path", input.message)
        if (second is PathResult.Error) return ToolResult.error("invalid_path", second.message)
        if (output is PathResult.Error) return ToolResult.error("invalid_path", output.message)
        val requiredError = when (action) {
            "inspect", "extract" -> if (input is PathResult.Missing) "input_path is required" else null
            "create" -> if (output is PathResult.Missing) "output_path is required" else null
            "reconcile" -> when {
                input is PathResult.Missing -> "input_path is required"
                second is PathResult.Missing -> "second_input_path is required"
                output is PathResult.Missing -> "output_path is required"
                else -> null
            }
            else -> null
        }
        if (requiredError != null) return ToolResult.error("missing_path", requiredError)
        listOfNotNull(
            (input as? PathResult.Valid)?.file,
            (second as? PathResult.Valid)?.file,
            (output as? PathResult.Valid)?.file,
        ).firstOrNull { !it.name.endsWith(".${plugin.extension}", ignoreCase = true) }?.let {
            return ToolResult.error("invalid_file_type", "${plugin.label} requires .${plugin.extension} files: $it")
        }

        val request = JSONObject()
            .put("plugin", plugin.id)
            .put("action", action)
            .put("input_path", (input as? PathResult.Valid)?.file?.absolutePath)
            .put("second_input_path", (second as? PathResult.Valid)?.file?.absolutePath)
            .put("output_path", (output as? PathResult.Valid)?.file?.absolutePath)
            .put("options", options)
        val result = withContext(Dispatchers.IO) {
            runCatching { OfficePluginManager.runEngine(context, request.toString()) }
                .getOrElse {
                    return@withContext OfficePluginManager.EngineResult(
                        125,
                        "",
                        "Office engine could not start: ${it.message ?: it.javaClass.simpleName}",
                    )
                }
        }
        return if (result.exitCode == 0) {
            ToolResult.success(mapOf("office_result_json" to result.stdout.trim()))
        } else {
            ToolResult.error("office_engine_failed", result.stderr.ifBlank { result.stdout }.take(8_000))
        }
    }

    private fun checkedPath(raw: String?, output: Boolean): PathResult {
        if (raw.isNullOrBlank()) return PathResult.Missing
        val file = runCatching { File(raw).canonicalFile }
            .getOrElse { return PathResult.Error("Invalid path: ${it.message}") }
        if (!output && !file.isFile) return PathResult.Error("Input file does not exist: $file")
        if (output && file.exists()) return PathResult.Error("Refusing to overwrite existing output: $file")
        val projects = File(context.filesDir, "home/projects").canonicalFile
        if (!DangerZonePolicy.load(context).allowOutsideProject &&
            file.path != projects.path && !file.path.startsWith(projects.path + File.separator)
        ) {
            return PathResult.Error("Path is outside Zdroid-B projects. Import the document into a project, or explicitly review Settings > Danger Zone.")
        }
        return PathResult.Valid(file)
    }

    private sealed interface PathResult {
        data object Missing : PathResult
        data class Valid(val file: File) : PathResult
        data class Error(val message: String) : PathResult
    }
}

enum class OfficePlugin(
    val id: String,
    val label: String,
    val extension: String,
    val actions: Set<String>,
    val toolDescription: String,
) {
    EXCEL("excel", "Excel Tools", "xlsx", setOf("inspect", "extract", "reconcile"), "Inspect and extract Excel workbooks or reconcile two workbooks into a validated new XLSX with status sheets and a pie chart."),
    WORD("word", "Word Tools", "docx", setOf("inspect", "extract", "create"), "Inspect, extract, or create validated DOCX documents without changing the source file."),
    POWERPOINT("powerpoint", "PowerPoint Tools", "pptx", setOf("inspect", "extract", "create"), "Inspect, extract, or create validated PPTX presentations without changing the source file."),
    PDF("pdf", "PDF Tools", "pdf", setOf("inspect", "extract", "create"), "Inspect, extract, or create validated PDF files without changing the source file."),
}

object OfficePluginManager {
    private const val ENGINE = "/data/data/com.zdroid/files/office-tools/office_tools.py"
    private const val PYTHON = "/root/.local/share/zdroid-office/venv/bin/python"

    fun isInstalled(context: Context, plugin: OfficePlugin) = marker(context, plugin, "installed").isFile
    fun isActive(context: Context, plugin: OfficePlugin) = isInstalled(context, plugin) && marker(context, plugin, "active").isFile
    data class EngineResult(val exitCode: Int, val stdout: String, val stderr: String)
    fun runEngine(context: Context, request: String): EngineResult {
        val output = File.createTempFile("office-output-", ".json", context.cacheDir)
        val error = File.createTempFile("office-error-", ".log", context.cacheDir)
        try {
            val process = ProcessBuilder("/data/data/com.zdroid/files/bin/zd-exec", PYTHON, ENGINE)
                .redirectOutput(output)
                .redirectError(error)
                .start()
            process.outputStream.bufferedWriter().use { it.write(request) }
            if (!process.waitFor(120, TimeUnit.SECONDS)) {
                process.destroyForcibly()
                return EngineResult(124, "", "Office operation timed out after 120 seconds")
            }
            if (output.length() > MAX_ENGINE_OUTPUT_BYTES) {
                return EngineResult(126, "", "Office result exceeded the ${MAX_ENGINE_OUTPUT_BYTES / 1_000_000} MB safety limit")
            }
            return EngineResult(
                process.exitValue(),
                output.readLimited(MAX_ENGINE_OUTPUT_BYTES.toInt()),
                error.readLimited(MAX_ENGINE_ERROR_CHARACTERS),
            )
        } finally {
            output.delete()
            error.delete()
        }
    }

    private fun marker(context: Context, plugin: OfficePlugin, suffix: String) =
        File(context.filesDir, "office-tools/${plugin.id}.$suffix")

    private fun File.readLimited(maxCharacters: Int): String =
        inputStream().bufferedReader().use { reader ->
            val buffer = CharArray(maxCharacters)
            val count = reader.read(buffer)
            if (count <= 0) "" else String(buffer, 0, count)
        }

    private const val MAX_ENGINE_OUTPUT_BYTES = 8_000_000L
    private const val MAX_ENGINE_ERROR_CHARACTERS = 8_000
    const val MAX_OPTIONS_CHARACTERS = 1_000_000
}
