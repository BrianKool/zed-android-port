use std::process::Command;

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, ScrollHandle, Window,
    prelude::*,
};
use theme::ActiveTheme;
use ui::{
    Button, Clickable, Color, Disableable, FluentBuilder, Headline, HeadlineSize, Icon, IconName,
    IconSize, Label, LabelCommon, LabelSize, ParentElement, SpinnerLabel, Styled, WithScrollbar,
    div, h_flex, v_flex, vh, vw,
};
use workspace::{DismissDecision, ModalView, Workspace, client_side_decorations};

const PREPARE_TASK_ID: &str = "zdroid-browser-tools-prepare";

pub fn register(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace.register_action(
                |workspace: &mut Workspace, _: &agent_ui::ConfigureBrowserTools, window, cx| {
                    workspace.toggle_modal(window, cx, |_, cx| BrowserToolsModal::new(cx));
                },
            );
        },
    )
    .detach();
}

struct BrowserToolsModal {
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    enabled: bool,
    status: String,
    working: bool,
}

impl BrowserToolsModal {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            enabled: super::android_browser_enabled_path().is_ok_and(|path| path.is_file()),
            status: super::android_browser_status(),
            working: false,
        }
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let launcher = match super::ensure_android_browser_launcher() {
            Ok(launcher) => launcher,
            Err(error) => {
                self.status = format!("Could not prepare launcher: {error:#}");
                cx.notify();
                return;
            }
        };
        if let Err(error) = super::set_android_browser_enabled(true) {
            self.status = format!("Could not enable Browser Tools: {error:#}");
            cx.notify();
            return;
        }

        self.enabled = true;
        self.working = true;
        self.status = "Preparing Android Browser Tools...".into();
        super::register_android_browser_context_server(launcher.clone(), cx);
        cx.start_background_task(PREPARE_TASK_ID, "Preparing Android Browser Tools");
        cx.notify();

        let prepare = cx.background_executor().spawn(async move {
            Command::new(launcher)
                .arg("--prepare")
                .output()
                .map_err(anyhow::Error::from)
        });
        cx.spawn(async move |this, cx| {
            let result = prepare.await;
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(output) if output.status.success() => {
                        this.status = super::android_browser_status();
                        cx.finish_background_task(
                            PREPARE_TASK_ID,
                            "Android Browser Tools are ready",
                            true,
                        );
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        this.status = if stderr.is_empty() {
                            super::android_browser_status()
                        } else {
                            stderr
                        };
                        cx.finish_background_task(
                            PREPARE_TASK_ID,
                            "Android Browser Tools setup failed",
                            false,
                        );
                    }
                    Err(error) => {
                        this.status = format!("Browser Tools setup failed: {error:#}");
                        cx.finish_background_task(
                            PREPARE_TASK_ID,
                            "Android Browser Tools setup failed",
                            false,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disable(&mut self, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        match super::set_android_browser_enabled(false) {
            Ok(()) => {
                super::unregister_android_browser_context_server(cx);
                self.enabled = false;
                self.status = super::android_browser_status();
            }
            Err(error) => self.status = format!("Could not disable Browser Tools: {error:#}"),
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enabled = super::android_browser_enabled_path().is_ok_and(|path| path.is_file());
        self.status = super::android_browser_status();
        cx.notify();
    }
}

impl Focusable for BrowserToolsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for BrowserToolsModal {}

impl ModalView for BrowserToolsModal {
    fn on_before_dismiss(&mut self, _: &mut Window, _: &mut Context<Self>) -> DismissDecision {
        DismissDecision::Dismiss(!self.working)
    }

    fn render_bare(&self) -> bool {
        cfg!(target_os = "android")
    }

    fn show_close_button(&self) -> bool {
        !self.working
    }

    fn android_full_size(&self) -> bool {
        true
    }
}

impl Render for BrowserToolsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let bg = colors.editor_background;
        let border = colors.border_variant;
        let compact = window.viewport_size().width.as_f32() < 520.0;

        let capability = |icon, title, detail: &'static str| {
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_3()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(Icon::new(icon).size(IconSize::Small).color(Color::Accent))
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(Label::new(title).size(LabelSize::Default))
                        .child(
                            Label::new(detail)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
        };

        let status_color = if self.status.starts_with("Ready") {
            Color::Success
        } else if self.status.starts_with("Error") || self.status.contains("failed") {
            Color::Error
        } else {
            Color::Muted
        };

        let content = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .p_6()
            .when(compact, |this| this.p_3().gap_3())
            .child(
                v_flex()
                    .gap_1()
                    .child(Headline::new("Android Browser & Phone Tools").size(HeadlineSize::Medium))
                    .child(
                        Label::new("Let Claude, Codex, or the Zed Agent inspect and control Android Chrome. Native Android controls are available as an explicit fallback.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(self.working, |row| row.child(SpinnerLabel::dots_variant()))
                            .child(Label::new(if self.enabled { "Enabled" } else { "Disabled" }).color(status_color)),
                    )
                    .child(Label::new(self.status.clone()).size(LabelSize::Small).color(status_color)),
            )
            .child(capability(
                IconName::Public,
                "Chrome automation",
                "Accessibility snapshots, tabs, navigation, click, type, scroll, downloads, and screenshots.",
            ))
            .child(capability(
                IconName::Image,
                "Vision fallback",
                "When a page cannot be understood structurally, the Agent can inspect a fresh screenshot and click the visible target.",
            ))
            .child(capability(
                IconName::Screen,
                "Native Android fallback",
                "Screen snapshot, screenshot, tap, swipe, text input, safe keys, and opening an app by package name.",
            ))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(Label::new("Before enabling").size(LabelSize::Default))
                    .child(Label::new("1. Enable Developer options and Wireless debugging.").size(LabelSize::Small).color(Color::Muted))
                    .child(Label::new("2. Pair this phone with local adb. Android must show the device as authorized.").size(LabelSize::Small).color(Color::Muted))
                    .child(Label::new("3. In Chrome flags, enable command line on non-rooted devices.").size(LabelSize::Small).color(Color::Muted))
                    .child(Label::new("4. Keep the screen awake while screenshots or UI actions run.").size(LabelSize::Small).color(Color::Muted)),
            )
            .child(
                Label::new("Browser and phone contents are private. Tools stay disabled until you enable them. Consequential actions still require confirmation. Playwright Android support is experimental.")
                    .size(LabelSize::XSmall)
                    .color(Color::Warning),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .when(compact, |row| row.flex_col().items_stretch())
                    .child(
                        Button::new(
                            "browser-tools-enable",
                            if self.enabled { "Retry / Repair" } else { "Enable & Prepare" },
                        )
                        .disabled(self.working)
                        .on_click(cx.listener(|this, _, _, cx| this.prepare(cx))),
                    )
                    .child(
                        Button::new("browser-tools-refresh", "Refresh status")
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .when(self.enabled, |row| {
                        row.child(
                            Button::new("browser-tools-disable", "Disable")
                                .disabled(self.working)
                                .on_click(cx.listener(|this, _, _, cx| this.disable(cx))),
                        )
                    }),
            );

        let scroll = div()
            .id("browser-tools-scroll")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(bg)
            .child(content);

        let modal = v_flex()
            .w(vw(0.9, window))
            .h(vh(0.9, window))
            .min_w_0()
            .bg(bg)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(scroll)
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            );

        client_side_decorations(modal, window, cx)
    }
}
