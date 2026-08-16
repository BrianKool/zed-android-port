use crate::{
    NewFile, Open, OpenMode, PathList, RecentWorkspace, SerializedWorkspaceLocation, Workspace,
    WorkspaceSettings,
    item::{Item, ItemEvent},
    persistence::WorkspaceDb,
};
use agent_settings::AgentSettings;
use git::Clone as GitClone;
use gpui::{
    Action, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, ParentElement, Render, Styled, Task, TaskExt, Window, actions,
};
use gpui::{WeakEntity, linear_color_stop, linear_gradient};
use menu::{SelectNext, SelectPrevious};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::{DefaultOpenBehavior, Settings};
use ui::{
    ButtonLike, CopyButton, Divider, DividerColor, IconButton, KeyBinding, Tooltip, Vector,
    VectorName, prelude::*,
};
use util::ResultExt;
use zed_actions::{OpenOnboarding, assistant::ToggleFocus, command_palette};

#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = welcome)]
#[serde(transparent)]
pub struct OpenRecentProject {
    pub index: usize,
}

actions!(
    zed,
    [
        /// Show the Zed welcome screen
        ShowWelcome
    ]
);

#[derive(IntoElement)]
struct SectionHeader {
    title: SharedString,
}

impl SectionHeader {
    fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .px_1()
            .mb_2()
            .gap_2()
            .child(
                Label::new(self.title.to_ascii_uppercase())
                    .buffer_font(cx)
                    .color(Color::Muted)
                    .size(LabelSize::XSmall),
            )
            .child(Divider::horizontal().color(DividerColor::BorderVariant))
    }
}

#[derive(IntoElement)]
struct SectionButton {
    label: SharedString,
    icon: IconName,
    action: Box<dyn Action>,
    tab_index: usize,
    focus_handle: FocusHandle,
}

impl SectionButton {
    fn new(
        label: impl Into<SharedString>,
        icon: IconName,
        action: &dyn Action,
        tab_index: usize,
        focus_handle: FocusHandle,
    ) -> Self {
        Self {
            label: label.into(),
            icon,
            action: action.boxed_clone(),
            tab_index,
            focus_handle,
        }
    }
}

impl RenderOnce for SectionButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = format!("onb-button-{}-{}", self.label, self.tab_index);
        let action_ref: &dyn Action = &*self.action;

        ButtonLike::new(id)
            .tab_index(self.tab_index as isize)
            .full_width()
            .size(ButtonSize::Medium)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Icon::new(self.icon)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new(self.label)),
                    )
                    .child(
                        KeyBinding::for_action_in(action_ref, &self.focus_handle, cx)
                            .size(rems_from_px(12.)),
                    ),
            )
            .on_click(move |_, window, cx| {
                self.focus_handle.dispatch_action(&*self.action, window, cx)
            })
    }
}

enum SectionVisibility {
    Always,
}

impl SectionVisibility {
    fn is_visible(&self) -> bool {
        match self {
            SectionVisibility::Always => true,
        }
    }
}

struct SectionEntry {
    icon: IconName,
    title: &'static str,
    action: &'static dyn Action,
    visibility_guard: SectionVisibility,
}

impl SectionEntry {
    fn render(&self, button_index: usize, focus: &FocusHandle) -> Option<impl IntoElement> {
        self.visibility_guard.is_visible().then(|| {
            SectionButton::new(
                self.title,
                self.icon,
                self.action,
                button_index,
                focus.clone(),
            )
        })
    }
}

const CONTENT: Section<4> = Section {
    title: "Get Started",
    entries: [
        SectionEntry {
            icon: IconName::Plus,
            title: "New File",
            action: &NewFile,
            visibility_guard: SectionVisibility::Always,
        },
        SectionEntry {
            icon: IconName::FolderOpen,
            title: "Open Project",
            action: &Open::DEFAULT,
            visibility_guard: SectionVisibility::Always,
        },
        SectionEntry {
            icon: IconName::CloudDownload,
            title: "Clone Repository",
            action: &GitClone,
            visibility_guard: SectionVisibility::Always,
        },
        SectionEntry {
            icon: IconName::ListCollapse,
            title: "Open Command Palette",
            action: &command_palette::Toggle,
            visibility_guard: SectionVisibility::Always,
        },
    ],
};

struct Section<const COLS: usize> {
    title: &'static str,
    entries: [SectionEntry; COLS],
}

impl<const COLS: usize> Section<COLS> {
    fn render(self, index_offset: usize, focus: &FocusHandle) -> impl IntoElement {
        v_flex()
            .min_w_full()
            .child(SectionHeader::new(self.title))
            .children(
                self.entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| entry.render(index_offset + index, focus)),
            )
    }
}

pub struct WelcomePage {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    fallback_to_recent_projects: bool,
    recent_workspaces: Option<Vec<RecentWorkspace>>,
    agent_setup_info_open: bool,
    agent_setup_info_compact: bool,
    terminal_tutorial_open: bool,
}

impl WelcomePage {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        fallback_to_recent_projects: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();

        if fallback_to_recent_projects {
            let fs = workspace
                .upgrade()
                .map(|ws| ws.read(cx).app_state().fs.clone());
            let db = WorkspaceDb::global(cx);
            cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
                let Some(fs) = fs else { return };
                let workspaces = db
                    .recent_project_workspaces(fs.as_ref())
                    .await
                    .log_err()
                    .unwrap_or_default();

                this.update(cx, |this, cx| {
                    this.recent_workspaces = Some(workspaces);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }

        WelcomePage {
            workspace,
            focus_handle,
            fallback_to_recent_projects,
            recent_workspaces: None,
            agent_setup_info_open: false,
            agent_setup_info_compact: false,
            terminal_tutorial_open: false,
        }
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
        cx.notify();
    }

    fn open_recent_project(
        &mut self,
        action: &OpenRecentProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(recent_workspaces) = &self.recent_workspaces {
            if let Some(workspace) = recent_workspaces.get(action.index) {
                let is_local = matches!(workspace.location, SerializedWorkspaceLocation::Local);

                if is_local {
                    let paths = workspace.paths.paths().to_vec();
                    let open_mode = match WorkspaceSettings::get_global(cx).default_open_behavior {
                        DefaultOpenBehavior::ExistingWindow => OpenMode::Activate,
                        DefaultOpenBehavior::NewWindow => OpenMode::NewWindow,
                    };
                    self.workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .open_workspace_for_paths(open_mode, paths, window, cx)
                                .detach_and_log_err(cx);
                        })
                        .log_err();
                } else {
                    use zed_actions::OpenRecent;
                    window.dispatch_action(OpenRecent::default().boxed_clone(), cx);
                }
            }
        }
    }

    fn render_agent_card(
        &self,
        tab_index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        let color = cx.theme().colors();

        let description = "Run multiple threads at once, mix and match any ACP-compatible agent, and keep work conflict-free with worktrees.";

        v_flex()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(color.border_variant)
            .bg(linear_gradient(
                360.,
                linear_color_stop(color.panel_background, 1.0),
                linear_color_stop(color.editor_background, 0.45),
            ))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::ZedAssistant)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new("Collaborate with Agents")),
                    )
                    .child(
                        IconButton::new("agent-setup-info", IconName::Info)
                            .icon_size(IconSize::Small)
                            .toggle_state(self.agent_setup_info_open)
                            .tooltip(Tooltip::text("Agent CLI setup"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.agent_setup_info_open = !this.agent_setup_info_open;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Label::new(description)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .mb_2(),
            )
            .when(self.agent_setup_info_open, |this| {
                this.child(self.render_agent_setup_info(window, cx))
            })
            .child(
                Button::new("open-agent", "Open Agent Panel")
                    .full_width()
                    .tab_index(tab_index as isize)
                    .style(ButtonStyle::Outlined)
                    .key_binding(
                        KeyBinding::for_action_in(&ToggleFocus, &self.focus_handle, cx)
                            .size(rems_from_px(12.)),
                    )
                    .on_click(move |_, window, cx| {
                        focus.dispatch_action(&ToggleFocus, window, cx);
                    }),
            )
    }

    fn open_android_runtime_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match cx.build_action("zdroid_runtime::PickRuntime", None) {
            Ok(action) => window.dispatch_action(action, cx),
            Err(err) => log::warn!("welcome: zdroid_runtime::PickRuntime is not registered: {err}"),
        }
    }

    fn render_terminal_tutorial_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let border_variant = colors.border_variant;
        let panel_background = colors.panel_background;
        let editor_background = colors.editor_background;
        let show_button_label = window.viewport_size().width >= px(600.0);

        v_flex()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(border_variant)
            .bg(panel_background)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::Terminal)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .child(Label::new("Zdroid Terminal Tutorial")),
                    )
                    .child(
                        IconButton::new("terminal-tutorial-toggle", IconName::Info)
                            .icon_size(IconSize::Small)
                            .toggle_state(self.terminal_tutorial_open)
                            .tooltip(Tooltip::text("Learn the Zdroid-B terminal runtime"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.terminal_tutorial_open = !this.terminal_tutorial_open;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Label::new(
                    "Learn how the terminal, Ubuntu runtime, Python tools, GitHub credentials and AI CLIs fit together.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted)
                .mb_2(),
            )
            .when(self.terminal_tutorial_open, |this| {
                let max_height = (window.viewport_size().height - px(220.0))
                    .max(px(220.0))
                    .min(px(560.0));

                this.child(
                    v_flex()
                        .id("terminal-tutorial-content")
                        .w_full()
                        .min_w_0()
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .gap_3()
                        .p_3()
                        .rounded_sm()
                        .border_1()
                        .border_color(border_variant)
                        .bg(editor_background)
                        .child(
                            Label::new("Runtime map")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "Zdroid-B uses one app sandbox with two cooperating runtimes. The Android Bootstrap keeps app-native tools, Node/npm and subscription agent launchers stable. The Ubuntu runtime is for normal development commands such as Python, pipx, apt packages and local servers.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(
                            Label::new("Check where you are")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(self.render_terminal_tutorial_command(
                            "terminal-check-runtime",
                            "echo $ZDROID_RUNTIME && which python3 && which pip && which codex && which claude && which gh",
                            cx,
                        ))
                        .child(
                            Label::new("Python and CLI apps")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "Use pipx for global Python CLI tools and virtual environments for project dependencies. Ubuntu follows PEP 668, so bare system-wide pip install is intentionally blocked.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(self.render_terminal_tutorial_command(
                            "terminal-python-pipx",
                            "pipx install graphifyy",
                            cx,
                        ))
                        .child(self.render_terminal_tutorial_command(
                            "terminal-python-venv",
                            "python3 -m venv .venv && source .venv/bin/activate && pip install -r requirements.txt",
                            cx,
                        ))
                        .child(
                            Label::new("Codex, Claude and GitHub")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "Codex, Claude and gh are bridged so terminal sign-in and the Agent Panel can use the same device credentials. If an agent cannot start, run its install or login action from the Agent setup info.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(self.render_terminal_tutorial_command(
                            "terminal-agent-login",
                            "codex login\nclaude\ngh auth login",
                            cx,
                        ))
                        .child(
                            Label::new("Repair and runtime tools")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "If Python, pipx, node, git, Codex or Claude reports missing files, open Android Runtime and use the repair cards. Python CLI Tools repairs pipx, venv and native build tools. Full Linux repairs the Ubuntu userland.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    Button::new("open-android-runtime", "Open Android Runtime")
                                        .style(ButtonStyle::Outlined)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_android_runtime_picker(window, cx);
                                        })),
                                )
                                .when(show_button_label, |this| {
                                    this.child(
                                        Label::new("Settings > Android Runtime > Repair")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        )
                        .child(
                            Label::new("Local servers")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "For web apps, start the server in the terminal and open localhost in the phone browser. Long-running commands continue as Zdroid-B background tasks and report completion through notifications.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(self.render_terminal_tutorial_command(
                            "terminal-local-server",
                            "npm run dev -- --host 0.0.0.0",
                            cx,
                        ))
                        .child(
                            Label::new("Common fixes")
                                .size(LabelSize::Small)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new(
                                "externally-managed-environment: use pipx or venv. python3.12 not found: repair Python CLI Tools. tree-sitter builds using android tags: open a new terminal after runtime setup and check ZDROID_RUNTIME. dubious ownership: press Trust Directory or set git safe.directory.",
                            )
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        ),
                )
            })
    }

    fn render_terminal_tutorial_command(
        &self,
        id: &'static str,
        text: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();

        h_flex()
            .id(id)
            .w_full()
            .min_w_0()
            .items_start()
            .justify_between()
            .gap_2()
            .p_2()
            .rounded_sm()
            .bg(colors.panel_background)
            .border_1()
            .border_color(colors.border_variant)
            .child(
                div().min_w_0().flex_1().child(
                    Label::new(text)
                        .buffer_font(cx)
                        .size(LabelSize::XSmall)
                        .color(Color::Default),
                ),
            )
            .child(
                CopyButton::new(format!("copy-{id}"), text)
                    .icon_size(IconSize::Small)
                    .tooltip_label("Copy command"),
            )
    }

    fn run_agent_setup_command(
        &mut self,
        id: &'static str,
        label: &'static str,
        command: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
        self.agent_setup_info_compact = true;
        cx.notify();

        let terminal_task = task::SpawnInTerminal {
            id: task::TaskId("zdroid-agent-setup".into()),
            full_label: "Zdroid-B Agent Setup".to_string(),
            label: label.to_string(),
            command: Some(command.to_string()),
            command_label: command.to_string(),
            use_new_terminal: false,
            allow_concurrent_runs: false,
            reveal: task::RevealStrategy::Always,
            reveal_target: zed_actions::RevealTarget::Dock,
            hide: task::HideStrategy::Never,
            shell: task::Shell::System,
            show_summary: true,
            show_command: true,
            ..Default::default()
        };

        let Ok(task) = self.workspace.update(cx, |workspace, cx| {
            workspace.spawn_in_terminal(terminal_task, window, cx)
        }) else {
            return;
        };

        let background_id = format!("zdroid-agent-setup-{id}");
        cx.start_background_task(&background_id, label);
        cx.spawn(async move |_, cx| {
            let result = task.await;
            let successful = matches!(result, Some(Ok(status)) if status.success());
            cx.update(|cx| {
                cx.finish_background_task(&background_id, label, successful);
            });
        })
        .detach();
    }

    fn render_agent_setup_info(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let show_install_label = window.viewport_size().width >= px(600.0);
        let max_height = if self.agent_setup_info_compact {
            if show_install_label {
                px(320.0)
            } else {
                px(210.0)
            }
        } else {
            (window.viewport_size().height - px(220.0))
                .max(px(180.0))
                .min(px(520.0))
        };
        let command = |id: &'static str,
                       label: &'static str,
                       text: &'static str,
                       install_command: Option<&'static str>| {
            let install_action = if show_install_label {
                Button::new(format!("install-{id}"), "Run")
                    .start_icon(Icon::new(IconName::PlayFilled).size(IconSize::Small))
                    .style(ButtonStyle::Outlined)
                    .disabled(install_command.is_none())
                    .tooltip(move |window, cx| {
                        Tooltip::text(if install_command.is_some() {
                            "Run in terminal"
                        } else {
                            "No verified Android installer is available"
                        })(window, cx)
                    })
                    .when_some(install_command, |button, install_command| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            this.run_agent_setup_command(id, label, install_command, window, cx);
                        }))
                    })
                    .into_any_element()
            } else {
                IconButton::new(format!("install-{id}"), IconName::PlayFilled)
                    .icon_size(IconSize::Small)
                    .disabled(install_command.is_none())
                    .tooltip(move |window, cx| {
                        Tooltip::text(if install_command.is_some() {
                            "Run in terminal"
                        } else {
                            "No verified Android installer is available"
                        })(window, cx)
                    })
                    .when_some(install_command, |button, install_command| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            this.run_agent_setup_command(id, label, install_command, window, cx);
                        }))
                    })
                    .into_any_element()
            };

            h_flex()
                .id(id)
                .w_full()
                .min_w_0()
                .items_start()
                .justify_between()
                .gap_2()
                .p_2()
                .rounded_sm()
                .bg(colors.editor_background)
                .border_1()
                .border_color(colors.border_variant)
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(
                            Label::new(label)
                                .size(LabelSize::XSmall)
                                .color(Color::Default),
                        )
                        .child(Label::new(text).buffer_font(cx).size(LabelSize::XSmall)),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .child(
                            CopyButton::new(format!("copy-{id}"), text)
                                .icon_size(IconSize::Small)
                                .tooltip_label("Copy command"),
                        )
                        .child(install_action),
                )
        };

        v_flex()
            .id("agent-setup-info-content")
            .w_full()
            .min_w_0()
            .max_h(max_height)
            .overflow_y_scroll()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .mb_3()
            .p_3()
            .gap_3()
            .rounded_sm()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.panel_background)
            .child(
                Label::new(
                    "Run these commands in the Zdroid-B terminal. Sign-in credentials stay on this device; API keys are not required for subscription login.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(Label::new("Base packages").size(LabelSize::Small))
            .child(command(
                "agent-command-repair",
                "Repair package state",
                "env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/apt\" --fix-broken install -y",
                Some("env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/apt\" --fix-broken install -y"),
            ))
            .child(command(
                "agent-command-update",
                "Update packages",
                "env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/pkg\" update -y",
                Some("env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/pkg\" update -y"),
            ))
            .child(command(
                "agent-command-upgrade",
                "Upgrade packages",
                "env LD_LIBRARY_PATH=\"$PREFIX/lib\" DEBIAN_FRONTEND=noninteractive \"$PREFIX/.zed/bin/pkg\" upgrade -y -o Dpkg::Options::=\"--force-confdef\" -o Dpkg::Options::=\"--force-confold\"",
                Some("env LD_LIBRARY_PATH=\"$PREFIX/lib\" DEBIAN_FRONTEND=noninteractive \"$PREFIX/.zed/bin/pkg\" upgrade -y -o Dpkg::Options::=\"--force-confdef\" -o Dpkg::Options::=\"--force-confold\""),
            ))
            .child(command(
                "agent-command-base-install",
                "Install base packages",
                "env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/pkg\" install -y nodejs-lts git",
                Some("env LD_LIBRARY_PATH=\"$PREFIX/lib\" \"$PREFIX/.zed/bin/pkg\" install -y nodejs-lts git"),
            ))
            .child(Label::new("Codex").size(LabelSize::Small))
            .child(command(
                "agent-command-codex-install",
                "Install Codex",
                "\"$PREFIX/.zed/bin/codex\" --version",
                Some("\"$PREFIX/.zed/bin/codex\" --version"),
            ))
            .child(command(
                "agent-command-codex-login",
                "Login to Codex",
                "\"$PREFIX/.zed/bin/codex\" login",
                Some("\"$PREFIX/.zed/bin/codex\" login"),
            ))
            .child(Label::new("Claude Code").size(LabelSize::Small))
            .child(command(
                "agent-command-claude-install",
                "Install Claude",
                "\"$PREFIX/bin/npm\" install --prefix \"$HOME/.local/share/zdroid/claude-code\" --no-save --force @anthropic-ai/claude-code@2.1.112 @agentclientprotocol/claude-agent-acp@0.64.2 && test -f \"$HOME/.local/share/zdroid/claude-code/node_modules/@anthropic-ai/claude-code/cli.js\" && ln -sf \"$PREFIX/.zed/bin/claude\" \"$PREFIX/bin/claude\" && \"$PREFIX/.zed/bin/claude\" --version",
                Some("\"$PREFIX/bin/npm\" install --prefix \"$HOME/.local/share/zdroid/claude-code\" --no-save --force @anthropic-ai/claude-code@2.1.112 @agentclientprotocol/claude-agent-acp@0.64.2 && test -f \"$HOME/.local/share/zdroid/claude-code/node_modules/@anthropic-ai/claude-code/cli.js\" && ln -sf \"$PREFIX/.zed/bin/claude\" \"$PREFIX/bin/claude\" && \"$PREFIX/.zed/bin/claude\" --version"),
            ))
            .child(command(
                "agent-command-claude-login",
                "Login to Claude",
                "\"$PREFIX/.zed/bin/claude\"",
                Some("\"$PREFIX/.zed/bin/claude\""),
            ))
    }

    fn render_recent_project_section(
        &self,
        recent_projects: Vec<impl IntoElement>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .child(SectionHeader::new("Recent Projects"))
            .children(recent_projects)
    }

    fn render_recent_project(
        &self,
        project_index: usize,
        tab_index: usize,
        location: &SerializedWorkspaceLocation,
        paths: &PathList,
    ) -> impl IntoElement {
        let name = project_name(paths);

        let (icon, title) = match location {
            SerializedWorkspaceLocation::Local => (IconName::Folder, name),
            SerializedWorkspaceLocation::Remote(_) => (IconName::Server, name),
        };

        SectionButton::new(
            title,
            icon,
            &OpenRecentProject {
                index: project_index,
            },
            tab_index,
            self.focus_handle.clone(),
        )
    }
}

impl Render for WelcomePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let first_section = CONTENT;
        let first_section_entries = first_section.entries.len();

        let ai_enabled = AgentSettings::get_global(cx).enabled(cx);

        let recent_projects_data: Vec<_> = self
            .recent_workspaces
            .as_ref()
            .into_iter()
            .flatten()
            .take(5)
            .collect();

        let showing_recent_projects =
            self.fallback_to_recent_projects && !recent_projects_data.is_empty();
        let mut next_tab_index = first_section_entries
            + if showing_recent_projects {
                recent_projects_data.len()
            } else {
                0
            };
        let second_section = if showing_recent_projects {
            #[cfg(target_os = "android")]
            {
                // Android-only split: ~/projects/* projects (built locally,
                // exec-mounted) vs anything else (typically /storage/emulated/0/*
                // SAF-picked, FUSE noexec). The two `rust` problem â€” same name
                // appearing twice in Recent Projects from different storage
                // tiers â€” is otherwise indistinguishable to the user.
                let workspace_root = util::env::workspace_root().map(|h| h.join("projects"));
                let mut workspace_entries: Vec<gpui::AnyElement> = Vec::new();
                let mut external_entries: Vec<gpui::AnyElement> = Vec::new();
                for (index, workspace) in recent_projects_data.iter().enumerate() {
                    let is_workspace = workspace_root
                        .as_ref()
                        .and_then(|root| {
                            workspace.paths.paths().first().map(|p| p.starts_with(root))
                        })
                        .unwrap_or(false);
                    let rendered = self
                        .render_recent_project(
                            index,
                            first_section_entries + index,
                            &workspace.location,
                            &workspace.identity_paths,
                        )
                        .into_any_element();
                    if is_workspace {
                        workspace_entries.push(rendered);
                    } else {
                        external_entries.push(rendered);
                    }
                }
                v_flex()
                    .w_full()
                    .gap_2()
                    .when(!workspace_entries.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .child(SectionHeader::new("Workspace"))
                                .children(workspace_entries),
                        )
                    })
                    .when(!external_entries.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .child(SectionHeader::new("External"))
                                .children(external_entries),
                        )
                    })
                    .into_any_element()
            }
            #[cfg(not(target_os = "android"))]
            {
                let recent_projects = recent_projects_data
                    .iter()
                    .enumerate()
                    .map(|(index, workspace)| {
                        self.render_recent_project(
                            index,
                            first_section_entries + index,
                            &workspace.location,
                            &workspace.identity_paths,
                        )
                    })
                    .collect::<Vec<_>>();
                self.render_recent_project_section(recent_projects)
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        let welcome_label = if self.fallback_to_recent_projects {
            "Welcome back to Zdroid-B"
        } else {
            "Welcome to Zdroid-B"
        };
        let compact = cfg!(target_os = "android") && window.viewport_size().width.as_f32() < 520.0;
        let info_panel_open = self.agent_setup_info_open || self.terminal_tutorial_open;

        h_flex()
            .key_context("Welcome")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_recent_project))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .justify_center()
            .child(
                v_flex()
                    .id("welcome-content")
                    .p_8()
                    .when(compact, |this| this.p_4())
                    .max_w_128()
                    .size_full()
                    .min_h_0()
                    .gap_6()
                    .when(compact, |this| this.gap_4())
                    .when(!compact, |this| this.justify_center())
                    .when(!info_panel_open, |this| this.overflow_y_scroll())
                    .when(info_panel_open, |this| this.overflow_y_hidden())
                    .when(!info_panel_open, |this| this.child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .mb_4()
                            .gap_4()
                            .when(compact, |this| this.flex_col().items_center().text_center())
                            .child(Vector::square(VectorName::ZedLogo, rems_from_px(45.)))
                            .child(
                                v_flex().min_w_0().child(Headline::new(welcome_label)).child(
                                    v_flex()
                                        .gap_1()
                                        .child(
                                            Label::new("The editor for what's next")
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .italic(),
                                        )
                                        .child(
                                            Label::new(
                                                "Linux-compatible ARM64 environment. Install the standard Linux version of terminal and npm CLI tools.",
                                            )
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                        ),
                                ),
                            ),
                    ))
                    .when(!info_panel_open, |this| {
                        this.child(first_section.render(Default::default(), &self.focus_handle))
                    })
                    .when(!info_panel_open, |this| this.child(second_section))
                    .child(self.render_terminal_tutorial_card(window, cx))
                    .when(ai_enabled && !showing_recent_projects, |this| {
                        let agent_tab_index = next_tab_index;
                        next_tab_index += 1;
                        this.child(self.render_agent_card(agent_tab_index, window, cx))
                    })
                    .when(
                        !self.fallback_to_recent_projects && !info_panel_open,
                        |this| {
                            this.child(
                                v_flex().gap_4().child(Divider::horizontal()).child(
                                    Button::new("welcome-exit", "Return to Onboarding")
                                        .tab_index(next_tab_index as isize)
                                        .full_width()
                                        .label_size(LabelSize::XSmall)
                                        .on_click(|_, window, cx| {
                                            window
                                                .dispatch_action(OpenOnboarding.boxed_clone(), cx);
                                        }),
                                ),
                            )
                        },
                    ),
            )
    }
}

impl EventEmitter<ItemEvent> for WelcomePage {}

impl Focusable for WelcomePage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for WelcomePage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Welcome".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("New Welcome Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(crate::item::ItemEvent)) {
        f(*event)
    }
}

impl crate::SerializableItem for WelcomePage {
    fn serialized_item_kind() -> &'static str {
        "WelcomePage"
    }

    fn cleanup(
        workspace_id: crate::WorkspaceId,
        alive_items: Vec<crate::ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<()>> {
        crate::delete_unloaded_items(
            alive_items,
            workspace_id,
            "welcome_pages",
            &persistence::WelcomePagesDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        _project: Entity<project::Project>,
        workspace: gpui::WeakEntity<Workspace>,
        workspace_id: crate::WorkspaceId,
        item_id: crate::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<Entity<Self>>> {
        if persistence::WelcomePagesDb::global(cx)
            .get_welcome_page(item_id, workspace_id)
            .ok()
            .is_some_and(|is_open| is_open)
        {
            Task::ready(Ok(
                cx.new(|cx| WelcomePage::new(workspace, false, window, cx))
            ))
        } else {
            Task::ready(Err(anyhow::anyhow!("No welcome page to deserialize")))
        }
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: crate::ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let db = persistence::WelcomePagesDb::global(cx);
        Some(cx.background_spawn(
            async move { db.save_welcome_page(item_id, workspace_id, true).await },
        ))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        event == &ItemEvent::UpdateTab
    }
}

mod persistence {
    use crate::WorkspaceDb;
    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };

    pub struct WelcomePagesDb(ThreadSafeConnection);

    impl Domain for WelcomePagesDb {
        const NAME: &str = stringify!(WelcomePagesDb);

        const MIGRATIONS: &[&str] = (&[sql!(
                    CREATE TABLE welcome_pages (
                        workspace_id INTEGER,
                        item_id INTEGER UNIQUE,
                        is_open INTEGER DEFAULT FALSE,

                        PRIMARY KEY(workspace_id, item_id),
                        FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                        ON DELETE CASCADE
                    ) STRICT;
        )]);
    }

    db::static_connection!(WelcomePagesDb, [WorkspaceDb]);

    impl WelcomePagesDb {
        query! {
            pub async fn save_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId,
                is_open: bool
            ) -> Result<()> {
                INSERT OR REPLACE INTO welcome_pages(item_id, workspace_id, is_open)
                VALUES (?, ?, ?)
            }
        }

        query! {
            pub fn get_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId
            ) -> Result<bool> {
                SELECT is_open
                FROM welcome_pages
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}

fn project_name(paths: &PathList) -> String {
    let joined = paths
        .paths()
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        "Untitled".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_name_empty() {
        let paths = PathList::new::<&str>(&[]);
        assert_eq!(project_name(&paths), "Untitled");
    }

    #[test]
    fn test_project_name_single() {
        let paths = PathList::new(&["/home/user/my-project"]);
        assert_eq!(project_name(&paths), "my-project");
    }

    #[test]
    fn test_project_name_multiple() {
        // PathList sorts lexicographically, so filenames appear in alpha order
        let paths = PathList::new(&["/home/user/zed", "/home/user/api"]);
        assert_eq!(project_name(&paths), "api, zed");
    }

    #[test]
    fn test_project_name_root_path_filtered() {
        // A bare root "/" has no file_name(), falls back to "Untitled"
        let paths = PathList::new(&["/"]);
        assert_eq!(project_name(&paths), "Untitled");
    }
}
