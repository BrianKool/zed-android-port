package com.zdroid

import android.content.Context
import android.util.AtomicFile
import java.io.File
import java.util.Properties

/** Shared, immediately-applied safety policy for Android agent execution. */
object DangerZonePolicy {
    const val OUTSIDE_PROJECT_PHRASE = "ai agent interact outside of project"
    const val BOOTSTRAP_PHRASE = "ai agent modify zdroid bootstrap runtime"

    data class State(
        val allowOutsideProject: Boolean = false,
        val protectBootstrapRuntime: Boolean = true,
        val allowRawUiFallback: Boolean = false,
        val allowRawTextInput: Boolean = false,
        val allowPasswordAssistance: Boolean = false,
        val allowArbitraryIntents: Boolean = false,
        val allowConsequentialActions: Boolean = false,
        val skipFinalConfirmations: Boolean = false,
    )

    private const val DIRECTORY = "policies"
    private const val FILE_NAME = "danger-zone.properties"

    fun load(context: Context): State {
        val file = policyFile(context)
        if (!file.isFile) return State()
        return runCatching {
            val properties = Properties().apply { file.inputStream().use(::load) }
            State(
                allowOutsideProject = properties.getProperty("allow_agent_outside_project") == "true",
                protectBootstrapRuntime = properties.getProperty("protect_bootstrap_runtime") != "false",
                allowRawUiFallback = properties.getProperty("allow_raw_ui_fallback") == "true",
                allowRawTextInput = properties.getProperty("allow_raw_text_input") == "true",
                allowPasswordAssistance = properties.getProperty("allow_password_assistance") == "true",
                allowArbitraryIntents = properties.getProperty("allow_arbitrary_intents") == "true",
                allowConsequentialActions = properties.getProperty("allow_consequential_actions") == "true",
                skipFinalConfirmations = properties.getProperty("skip_mobile_final_confirmations") == "true",
            )
        }.getOrDefault(State())
    }

    fun save(context: Context, state: State): Boolean = runCatching {
        val file = policyFile(context)
        check(file.parentFile?.let { it.isDirectory || it.mkdirs() } == true) {
            "Could not create the Zdroid-B policy directory"
        }
        val properties = Properties().apply {
            setProperty("allow_agent_outside_project", state.allowOutsideProject.toString())
            setProperty("protect_bootstrap_runtime", state.protectBootstrapRuntime.toString())
            setProperty("allow_raw_ui_fallback", state.allowRawUiFallback.toString())
            setProperty("allow_raw_text_input", state.allowRawTextInput.toString())
            setProperty("allow_password_assistance", state.allowPasswordAssistance.toString())
            setProperty("allow_arbitrary_intents", state.allowArbitraryIntents.toString())
            setProperty("allow_consequential_actions", state.allowConsequentialActions.toString())
            setProperty("skip_mobile_final_confirmations", state.skipFinalConfirmations.toString())
        }
        val atomicFile = AtomicFile(file)
        val output = atomicFile.startWrite()
        try {
            properties.store(output, "Zdroid-B danger-zone policy")
            atomicFile.finishWrite(output)
        } catch (error: Throwable) {
            atomicFile.failWrite(output)
            throw error
        }
        file.setReadable(false, false)
        file.setWritable(false, false)
        file.setReadable(true, true)
        file.setWritable(true, true)
    }.isSuccess

    fun activeProjectRoots(context: Context): List<File> =
        File(context.filesDir, "$DIRECTORY/active-project-roots.txt")
            .takeIf(File::isFile)
            ?.readLines()
            ?.mapNotNull { raw ->
                raw.trim().takeIf(String::isNotEmpty)?.let { runCatching { File(it).canonicalFile }.getOrNull() }
            }
            .orEmpty()

    private fun policyFile(context: Context) = File(context.filesDir, "$DIRECTORY/$FILE_NAME")
}
