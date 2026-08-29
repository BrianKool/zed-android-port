package com.zdroid

import android.content.Context
import android.os.Bundle
import android.view.accessibility.AccessibilityNodeInfo
import io.droidmcp.accessibility.AccessibilityServiceHolder
import io.droidmcp.core.McpTool
import io.droidmcp.core.ParameterType
import io.droidmcp.core.ToolAnnotations
import io.droidmcp.core.ToolParameter
import io.droidmcp.core.ToolResult
import java.util.Locale

/** The only route from an Agent to non-semantic Accessibility operations. */
class ControlledUiFallbackTool(
    private val context: Context,
    delegates: List<McpTool>,
) : McpTool {
    private val delegatesByName = delegates.associateBy(McpTool::name)
    override val name = "controlled_ui_fallback"
    override val description = "Use a controlled raw Accessibility fallback only after semantic UI returned a concrete failure. Password text is permanently blocked. Consequential actions require a user-approved confirmation token unless explicitly disabled in Mobile Use Danger Zone."
    private val controlParameters = listOf(
        ToolParameter("operation", "QUERY_SCREEN, SCREENSHOT, FIND_NODE, TAP, GESTURE, CLICK_NODE, LONG_CLICK_NODE, SCROLL_NODE, GLOBAL_ACTION, FIND_AND_TAP, SCROLL_TO_FIND, or WAIT_FOR_TEXT.", ParameterType.STRING, true),
        ToolParameter("semantic_failure", "The concrete observe_semantic_ui or perform_semantic_action failure that requires fallback.", ParameterType.STRING, true),
        ToolParameter("consequential", "True for payment, submit, delete, publish, account, or irreversible actions.", ParameterType.BOOLEAN, false),
        ToolParameter("confirmation_token", "Token returned after the user approves the final notification.", ParameterType.STRING, false),
    )
    override val parameters = controlParameters + delegatesByName.values
        .flatMap(McpTool::parameters)
        .filterNot { candidate -> controlParameters.any { it.name == candidate.name } }
        .distinctBy(ToolParameter::name)
    override val annotations = ToolAnnotations(destructiveHint = true, title = "Controlled Mobile Use fallback")

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        val policy = DangerZonePolicy.load(context)
        if (!policy.allowRawUiFallback) return ToolResult.error("raw_fallback_disabled", "Enable controlled raw UI fallback in Mobile Use Danger Zone")
        if (params["semantic_failure"]?.toString().isNullOrBlank()) {
            return ToolResult.error("semantic_first", "A concrete semantic UI failure is required")
        }
        val operation = params["operation"]?.toString()?.uppercase(Locale.ROOT).orEmpty()
        val delegateName = OPERATIONS[operation]
            ?: return ToolResult.error("unsupported_fallback", operation)
        val consequential = params["consequential"] as? Boolean == true ||
            operation in UNKNOWN_TARGET_MUTATIONS ||
            CONSEQUENTIAL_WORDS.any { word -> params.values.any { it.toString().contains(word, true) } }
        if (consequential) {
            if (!policy.allowConsequentialActions) {
                return ToolResult.error("consequential_actions_disabled", "Enable consequential Mobile Use actions first")
            }
            if (!policy.skipFinalConfirmations) {
                val supplied = params["confirmation_token"]?.toString()
                if (!MobileActionConfirmation.consumeApproved(supplied, delegateName)) {
                    val token = MobileActionConfirmation.request(context, delegateName)
                    return ToolResult.error("confirmation_required", "Approve notification, then retry with confirmation_token=$token")
                }
            }
        }
        val delegate = delegatesByName[delegateName]
            ?: return ToolResult.error("fallback_unavailable", delegateName)
        return delegate.execute(params - setOf("operation", "semantic_failure", "consequential", "confirmation_token"))
    }

    companion object {
        private val OPERATIONS = mapOf(
            "QUERY_SCREEN" to "query_screen",
            "SCREENSHOT" to "take_screenshot_via_a11y",
            "FIND_NODE" to "find_node",
            "TAP" to "tap",
            "GESTURE" to "gesture",
            "CLICK_NODE" to "click_node",
            "LONG_CLICK_NODE" to "long_click_node",
            "SCROLL_NODE" to "scroll_node",
            "GLOBAL_ACTION" to "global_action",
            "FIND_AND_TAP" to "find_and_tap",
            "SCROLL_TO_FIND" to "scroll_to_find",
            "WAIT_FOR_TEXT" to "wait_for_text",
        )
        private val UNKNOWN_TARGET_MUTATIONS = setOf(
            "TAP", "GESTURE", "CLICK_NODE", "LONG_CLICK_NODE", "FIND_AND_TAP",
        )
        private val CONSEQUENTIAL_WORDS = setOf(
            "pay", "purchase", "checkout", "submit", "publish", "delete", "transfer",
            "付款", "購買", "結帳", "提交", "發布", "刪除", "轉帳",
        )
    }
}

/** Keeps arbitrary intents behind both the Danger Zone grant and a final user decision. */
class ControlledIntentTool(
    private val context: Context,
    private val delegate: McpTool,
) : McpTool {
    override val name = delegate.name
    override val description = "Danger Zone intent. Final user confirmation is required unless explicitly disabled. ${delegate.description}"
    override val parameters = delegate.parameters + ToolParameter(
        "confirmation_token",
        "Token returned after approving the final notification.",
        ParameterType.STRING,
        false,
    )
    override val annotations = ToolAnnotations(destructiveHint = true, title = "Controlled Android intent")

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        val policy = DangerZonePolicy.load(context)
        if (!policy.allowArbitraryIntents) {
            return ToolResult.error("arbitrary_intents_disabled", "Enable arbitrary intents in Mobile Use Danger Zone")
        }
        val operation = "intent_${delegate.name}"
        if (!policy.skipFinalConfirmations &&
            !MobileActionConfirmation.consumeApproved(params["confirmation_token"]?.toString(), operation)
        ) {
            val token = MobileActionConfirmation.request(context, operation)
            return ToolResult.error("confirmation_required", "Approve notification, then retry with confirmation_token=$token")
        }
        return delegate.execute(params - "confirmation_token")
    }
}

/** Raw text fallback with a host-side password check before ACTION_SET_TEXT. */
class ControlledRawTextTool(private val context: Context) : McpTool {
    override val name = "controlled_raw_text_input"
    override val description = "Set text on a non-password editable Accessibility node. Password fields are permanently rejected; use secure password assistance instead."
    override val parameters = listOf(
        ToolParameter("replacement_text", "Replacement text.", ParameterType.STRING, true),
        ToolParameter("text", "Optional node label substring.", ParameterType.STRING, false),
        ToolParameter("view_id", "Optional exact view ID.", ParameterType.STRING, false),
        ToolParameter("class_name", "Optional exact class name.", ParameterType.STRING, false),
        ToolParameter("package_name", "Optional exact package name.", ParameterType.STRING, false),
        ToolParameter("index", "Zero-based match index.", ParameterType.INTEGER, false),
    )
    override val annotations = ToolAnnotations(destructiveHint = true, title = "Controlled raw text input")

    override suspend fun execute(params: Map<String, Any>): ToolResult {
        if (!DangerZonePolicy.load(context).allowRawTextInput) {
            return ToolResult.error("raw_text_disabled", "Enable raw text input in Mobile Use Danger Zone")
        }
        val replacement = params["replacement_text"]?.toString()
            ?: return ToolResult.error("replacement_text is required")
        val selectors = listOf("text", "view_id", "class_name", "package_name")
        if (selectors.none { !params[it]?.toString().isNullOrBlank() }) {
            return ToolResult.error("selector_required", "At least one selector is required")
        }
        val service = AccessibilityServiceHolder.service
            ?: return ToolResult.error("accessibility_not_enabled", null)
        val root = service.rootInActiveWindow ?: return ToolResult.error("ui_unavailable", null)
        val index = (params["index"] as? Number)?.toInt()?.coerceAtLeast(0) ?: 0
        val node = matchingNodes(root, params).drop(index).firstOrNull()
            ?: return ToolResult.error("no_match", "No editable node matched")
        if (node.isPassword) return ToolResult.error("secure_field", "Raw password-field text is permanently blocked")
        if (!node.isEditable) return ToolResult.error("node_not_editable", null)
        val arguments = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, replacement)
        }
        return if (node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments)) {
            ToolResult.success(mapOf("performed" to true, "password_field" to false))
        } else ToolResult.error("action_failed", "The target rejected ACTION_SET_TEXT")
    }

    private fun matchingNodes(root: AccessibilityNodeInfo, params: Map<String, Any>): Sequence<AccessibilityNodeInfo> = sequence {
        val pending = ArrayDeque<AccessibilityNodeInfo>()
        pending.add(root)
        while (pending.isNotEmpty()) {
            val node = pending.removeFirst()
            val label = listOf(node.text, node.contentDescription, node.hintText).joinToString(" ")
            val matches = params["text"]?.toString()?.let { label.contains(it, true) } ?: true
            val idMatches = params["view_id"]?.toString()?.let { node.viewIdResourceName == it } ?: true
            val classMatches = params["class_name"]?.toString()?.let { node.className?.toString() == it } ?: true
            val packageMatches = params["package_name"]?.toString()?.let { node.packageName?.toString() == it } ?: true
            if (matches && idMatches && classMatches && packageMatches) yield(node)
            for (child in 0 until node.childCount) node.getChild(child)?.let(pending::addLast)
        }
    }
}
