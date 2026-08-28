package com.zdroid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneActionPlanTest {
    @Test
    fun down_scroll_moves_the_finger_up_to_reveal_lower_content() {
        val points = actionPlanScrollPoints("down", 1_000f, 2_000f)!!
        assertEquals(500f, points[0][0])
        assertTrue(points[0][1] > points[1][1])
    }

    @Test
    fun forward_and_backward_use_reading_direction_aliases() {
        assertEquals(
            actionPlanScrollPoints("down", 1_000f, 2_000f),
            actionPlanScrollPoints("forward", 1_000f, 2_000f),
        )
        assertEquals(
            actionPlanScrollPoints("up", 1_000f, 2_000f),
            actionPlanScrollPoints("backward", 1_000f, 2_000f),
        )
    }

    @Test
    fun unknown_scroll_direction_is_rejected() {
        assertNull(actionPlanScrollPoints("diagonal", 1_000f, 2_000f))
    }

    @Test
    fun reflex_parser_understands_contextual_follow_up_scroll() {
        assertEquals(
            PhoneControlSession.Direction.DOWN,
            PhoneControlSession.directionForCommand("再滑一下"),
        )
    }

    @Test
    fun semantic_roles_are_platform_neutral() {
        assertEquals("TEXTBOX", SemanticUiRegistry.semanticRole("android.widget.EditText"))
        assertEquals("BUTTON", SemanticUiRegistry.semanticRole("android.widget.ImageButton"))
        assertEquals("SCROLL_AREA", SemanticUiRegistry.semanticRole("android.widget.ScrollView"))
    }

    @Test
    fun semantic_observe_options_are_bounded_and_invalid_modes_fall_back() {
        val options = SemanticObserveOptions.from(
            mapOf(
                "max_elements" to 10_000,
                "max_label_chars" to 4,
                "mode" to "not-a-mode",
                "region" to "content",
            ),
        )
        assertEquals(500, options.maxElements)
        assertEquals(32, options.maxLabelChars)
        assertEquals(SemanticObserveMode.COMPACT, options.mode)
        assertEquals("CONTENT", options.region)
    }

    @Test
    fun browser_router_keeps_signed_in_and_consequential_work_on_live_surface() {
        assertEquals(
            BrowserUseRouterTool.Route.LIVE_SEMANTIC,
            browserUseRoute("READ", true, signedIn = true, consequential = false, crawl4aiReady = true),
        )
        assertEquals(
            BrowserUseRouterTool.Route.LIVE_SEMANTIC,
            browserUseRoute("READ", true, signedIn = false, consequential = true, crawl4aiReady = true),
        )
    }

    @Test
    fun browser_router_uses_crawl4ai_only_for_explicit_public_read() {
        assertTrue(isPotentiallyPublicWebUrlSyntax("https://example.com/path"))
        assertTrue(!isPotentiallyPublicWebUrlSyntax("file:///data/local/private"))
        assertTrue(!isPotentiallyPublicWebUrlSyntax("http://127.0.0.1:8080/private"))
        assertTrue(!isPotentiallyPublicWebUrlSyntax("http://192.168.1.4/private"))
        assertEquals(
            BrowserUseRouterTool.Route.CRAWL4AI_READ,
            browserUseRoute("READ", true, signedIn = false, consequential = false, crawl4aiReady = true),
        )
    }
}
