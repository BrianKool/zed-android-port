use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AsyncApp, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    ScrollHandle, Task, WeakEntity, Window, div, prelude::*,
};
use theme::ActiveTheme;
use ui::{
    Button, Clickable, Color, Disableable, Headline, HeadlineSize, Icon, IconName, IconSize, Label,
    LabelCommon, LabelSize, ParentElement, ScrollAxes, Scrollbars, Styled, Switch, ToggleState,
    WithScrollbar, h_flex, v_flex,
};
use workspace::{DismissDecision, ModalView, Workspace};

const CONTEXT_SERVER_ID: &str = "zdroid-phone-use";
const PROXY_SOURCE: &str = include_str!("phone_use_mcp_proxy.mjs");
const OFFICE_ENGINE_SOURCE: &str = include_str!("office_tools.py");
const CRAWL4AI_VERSION: &str = "0.9.2";
const CRAWL4AI_TASK_ID: &str = "zdroid-crawl4ai-install";

#[derive(Clone, Copy)]
struct OfficePluginSpec {
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    packages: &'static str,
}

const OFFICE_PLUGINS: [OfficePluginSpec; 4] = [
    OfficePluginSpec {
        id: "excel",
        title: "Excel Tools",
        detail: "Inspect and extract XLSX files, reconcile two workbooks, and create a validated result workbook with status sheets and charts.",
        packages: "'openpyxl>=3.1,<4' 'XlsxWriter>=3.2,<4' 'polars>=1,<2'",
    },
    OfficePluginSpec {
        id: "word",
        title: "Word Tools",
        detail: "Inspect, extract, and create validated DOCX documents with paragraphs, headings, and tables.",
        packages: "'python-docx>=1.1,<2'",
    },
    OfficePluginSpec {
        id: "powerpoint",
        title: "PowerPoint Tools",
        detail: "Inspect, extract, and create validated PPTX slide decks.",
        packages: "'python-pptx>=1,<2'",
    },
    OfficePluginSpec {
        id: "pdf",
        title: "PDF Tools",
        detail: "Inspect and extract PDF pages or generate a validated PDF document.",
        packages: "'pypdf>=5,<7' 'reportlab>=4,<5'",
    },
];

struct PhoneUseContextServerDescriptor {
    launcher: PathBuf,
}

impl project::context_server_store::registry::ContextServerDescriptor
    for PhoneUseContextServerDescriptor
{
    fn command(
        &self,
        _worktree_store: Entity<project::worktree_store::WorktreeStore>,
        _cx: &AsyncApp,
    ) -> Task<Result<context_server::ContextServerCommand>> {
        Task::ready(Ok(context_server::ContextServerCommand {
            path: self.launcher.clone(),
            args: Vec::new(),
            env: None,
            timeout: Some(30_000),
        }))
    }

    fn configuration(
        &self,
        _worktree_store: Entity<project::worktree_store::WorktreeStore>,
        _cx: &AsyncApp,
    ) -> Task<Result<Option<extension::ContextServerConfiguration>>> {
        Task::ready(Ok(None))
    }
}

fn phone_use_paths() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let (prefix, home) = super::zdroid_bootstrap_paths()?;
    let runtime = prefix
        .parent()
        .context("Zdroid runtime prefix has no files directory")?
        .to_path_buf();
    Ok((prefix, home, runtime.join("phone-use")))
}

fn status() -> String {
    phone_use_paths()
        .ok()
        .and_then(|(_, _, runtime)| std::fs::read_to_string(runtime.join("status")).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Starting the Android-native Phone Use service...".into())
}

fn crawl4ai_marker_path() -> Option<PathBuf> {
    phone_use_paths()
        .ok()
        .map(|(_, home, _)| home.join(".config/zdroid/crawl4ai-version"))
}

fn crawl4ai_status() -> String {
    crawl4ai_marker_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|version| format!("Ready - Crawl4AI {}", version.trim()))
        .unwrap_or_else(|| "Not installed - optional Full Linux backend".into())
}

fn office_marker(plugin: OfficePluginSpec, suffix: &str) -> Option<PathBuf> {
    phone_use_paths().ok().map(|(prefix, _, _)| {
        prefix
            .parent()
            .unwrap_or(&prefix)
            .join("office-tools")
            .join(format!("{}.{}", plugin.id, suffix))
    })
}

fn office_installed(plugin: OfficePluginSpec) -> bool {
    office_marker(plugin, "installed").is_some_and(|path| path.is_file())
}

fn office_active(plugin: OfficePluginSpec) -> bool {
    office_installed(plugin) && office_marker(plugin, "active").is_some_and(|path| path.is_file())
}

fn remove_legacy_browser_runtime(home: &std::path::Path, prefix: &std::path::Path) {
    let legacy_launcher = prefix.join(".zed/bin/zdroid-playwright-android-mcp");
    let legacy_marker = home.join(".config/zdroid/browser-tools.enabled");
    let legacy_data = home.join(".local/share/zdroid/browser-tools");
    for path in [legacy_launcher, legacy_marker] {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "failed to remove legacy browser-tools file {}: {error}",
                path.display()
            );
        }
    }
    if let Err(error) = std::fs::remove_dir_all(&legacy_data)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!(
            "failed to remove legacy browser-tools directory {}: {error}",
            legacy_data.display()
        );
    }
}

fn ensure_launcher() -> Result<PathBuf> {
    let (prefix, home, runtime) = phone_use_paths()?;
    remove_legacy_browser_runtime(&home, &prefix);
    let managed_bin = prefix.join(".zed/bin");
    std::fs::create_dir_all(&managed_bin).context("create Phone Use launcher directory")?;
    std::fs::create_dir_all(&runtime).context("create Phone Use runtime directory")?;
    let office_runtime = prefix
        .parent()
        .context("Zdroid prefix has no files directory")?
        .join("office-tools");
    std::fs::create_dir_all(&office_runtime).context("create Office Tools runtime directory")?;
    std::fs::write(office_runtime.join("office_tools.py"), OFFICE_ENGINE_SOURCE)
        .context("write Office Tools engine")?;

    let proxy = runtime.join("phone_use_mcp_proxy.mjs");
    std::fs::write(&proxy, PROXY_SOURCE).context("write Phone Use MCP proxy")?;
    let launcher = managed_bin.join("zdroid-phone-use-mcp");
    let script = format!(
        r#"#!/system/bin/sh
set -eu
PREFIX="{prefix}"
token_file="{token_file}"
proxy="{proxy}"
NODE="$PREFIX/bin/node"

if [ ! -x "$NODE" ]; then
    printf '%s\n' 'Zdroid-B Phone Use: Node.js is not ready. Finish initialization and retry.' >&2
    exit 127
fi

waited=0
while [ ! -s "$token_file" ] && [ "$waited" -lt 100 ]; do
    sleep 0.1
    waited=$((waited + 1))
done
if [ ! -s "$token_file" ]; then
    printf '%s\n' 'Zdroid-B Phone Use: native service did not initialize.' >&2
    exit 69
fi

export ZDROID_PHONE_USE_TOKEN="$(cat "$token_file")"
export ZDROID_PHONE_USE_ENDPOINT="http://127.0.0.1:8765/mcp"
exec "$NODE" "$proxy"
"#,
        prefix = prefix.to_string_lossy(),
        token_file = runtime.join("token").to_string_lossy(),
        proxy = proxy.to_string_lossy(),
    );
    std::fs::write(&launcher, script).context("write Phone Use launcher")?;
    let mut permissions = std::fs::metadata(&launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&launcher, permissions).context("chmod Phone Use launcher")?;
    Ok(launcher)
}

pub fn initialize(cx: &mut App) {
    let launcher = match ensure_launcher() {
        Ok(launcher) => launcher,
        Err(error) => {
            log::error!("failed to prepare Android-native Phone Use: {error:#}");
            return;
        }
    };
    project::context_server_store::registry::ContextServerDescriptorRegistry::default_global(cx)
        .update(cx, |registry, cx| {
            registry.register_context_server_descriptor(
                CONTEXT_SERVER_ID.into(),
                Arc::new(PhoneUseContextServerDescriptor { launcher }),
                cx,
            );
        });
}

pub fn register(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace.register_action(
                |workspace: &mut Workspace, _: &agent_ui::ConfigurePhoneUse, window, cx| {
                    let workspace_entity = cx.weak_entity();
                    workspace.toggle_modal(window, cx, move |_, cx| {
                        PhoneUseModal::new(workspace_entity, cx)
                    });
                },
            );
        },
    )
    .detach();
}

struct PhoneUseModal {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    status: String,
    crawl4ai_status: String,
    crawl4ai_installing: bool,
    office_installing: Option<&'static str>,
}

impl PhoneUseModal {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            status: status(),
            crawl4ai_status: crawl4ai_status(),
            crawl4ai_installing: false,
            office_installing: None,
        }
    }

    fn install_crawl4ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.crawl4ai_installing {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            self.crawl4ai_status = "Open a workspace before installing Crawl4AI".into();
            cx.notify();
            return;
        };

        let command = format!(
            "python3.12 -m pipx install --force 'crawl4ai=={CRAWL4AI_VERSION}' && \"$HOME/.local/bin/crawl4ai-setup\""
        );
        let terminal_task = task::SpawnInTerminal {
            id: task::TaskId(CRAWL4AI_TASK_ID.into()),
            full_label: "Install Crawl4AI DOM-first browser backend".into(),
            label: "Installing Crawl4AI".into(),
            command: Some(command.clone()),
            command_label: command,
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
        let task = workspace.update(cx, |workspace, cx| {
            workspace.spawn_in_terminal(terminal_task, window, cx)
        });

        self.crawl4ai_installing = true;
        self.crawl4ai_status = "Installing Crawl4AI and its browser runtime...".into();
        cx.start_background_task(CRAWL4AI_TASK_ID, "Installing Crawl4AI browser backend");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let successful = matches!(result, Some(Ok(status)) if status.success());
            if successful && let Some(marker) = crawl4ai_marker_path() {
                if let Some(parent) = marker.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(marker, CRAWL4AI_VERSION);
            }
            let _ = this.update(cx, |this, cx| {
                this.crawl4ai_installing = false;
                this.crawl4ai_status = if successful {
                    crawl4ai_status()
                } else {
                    "Installation failed - review the terminal output and retry".into()
                };
                cx.finish_background_task(
                    CRAWL4AI_TASK_ID,
                    if successful {
                        "Crawl4AI browser backend installed"
                    } else {
                        "Crawl4AI browser backend installation failed"
                    },
                    successful,
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn install_office(
        &mut self,
        plugin: OfficePluginSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.office_installing.is_some() {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let task_id = format!("zdroid-office-{}-install", plugin.id);
        let (Some(marker), Some(active)) = (
            office_marker(plugin, "installed"),
            office_marker(plugin, "active"),
        ) else {
            self.status = "Office plugin paths are unavailable until Zdroid setup finishes".into();
            cx.notify();
            return;
        };
        let command = format!(
            "set -e; VENV=\"$HOME/.local/share/zdroid-office/venv\"; mkdir -p \"$VENV\" '{}'; if ! python3 -m venv \"$VENV\"; then apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y python3-venv python3-pip; python3 -m venv \"$VENV\"; fi; \"$VENV/bin/python\" -m pip install --upgrade 'pip>=24,<27'; \"$VENV/bin/pip\" install --upgrade {}; printf '1' > '{}'; printf '1' > '{}'",
            marker.parent().unwrap_or(&marker).display(),
            plugin.packages,
            marker.display(),
            active.display(),
        );
        let terminal_task = task::SpawnInTerminal {
            id: task::TaskId(task_id.clone().into()),
            full_label: format!("Install {}", plugin.title),
            label: format!("Installing {}", plugin.title),
            command: Some(command.clone()),
            command_label: command,
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
        let task = workspace.update(cx, |workspace, cx| {
            workspace.spawn_in_terminal(terminal_task, window, cx)
        });
        self.office_installing = Some(plugin.id);
        let installing_message = format!("Installing {}", plugin.title);
        cx.start_background_task(&task_id, &installing_message);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let successful =
                matches!(result, Some(Ok(status)) if status.success()) && office_installed(plugin);
            let _ = this.update(cx, |this, cx| {
                this.office_installing = None;
                let finished_message = if successful {
                    format!("{} installed", plugin.title)
                } else {
                    format!("{} installation failed", plugin.title)
                };
                cx.finish_background_task(&task_id, &finished_message, successful);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_office_active(
        &mut self,
        plugin: OfficePluginSpec,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if !office_installed(plugin) {
            return;
        }
        if let Some(path) = office_marker(plugin, "active") {
            if active {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, "1");
            } else if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("failed to deactivate {}: {error}", plugin.title);
            }
        }
        cx.notify();
    }
}

impl Focusable for PhoneUseModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for PhoneUseModal {}

impl ModalView for PhoneUseModal {
    fn on_before_dismiss(&mut self, _: &mut Window, _: &mut Context<Self>) -> DismissDecision {
        DismissDecision::Dismiss(true)
    }

    fn render_bare(&self) -> bool {
        cfg!(target_os = "android")
    }

    fn android_full_size(&self) -> bool {
        true
    }
}

impl Render for PhoneUseModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        self.status = status();
        let ready = self.status.starts_with("Ready");
        if !self.crawl4ai_installing {
            self.crawl4ai_status = crawl4ai_status();
        }
        let crawl4ai_ready = self.crawl4ai_status.starts_with("Ready");
        let capability = |icon, title, detail: &'static str| {
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_3()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(colors.border_variant)
                .child(Icon::new(icon).size(IconSize::Small).color(Color::Accent))
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(Label::new(title))
                        .child(
                            Label::new(detail)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
        };

        v_flex()
            .size_full()
            .min_w_0()
            .bg(colors.editor_background)
            .child(
                v_flex()
                    .id("phone-use-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .p_4()
                    .gap_4()
                    .child(Headline::new("Plugins").size(HeadlineSize::Medium))
                    .child(Label::new("Install optional capabilities once, then use their switch to control whether Agents may call them. Installed files are retained when a plugin is inactive.").color(Color::Muted))
                    .child(Headline::new("Mobile & Browser").size(HeadlineSize::Small))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(if ready { IconName::Check } else { IconName::Warning }).color(if ready { Color::Success } else { Color::Warning }))
                            .child(
                                div().min_w_0().flex_1().child(
                                    Label::new(self.status.clone()).color(if ready {
                                        Color::Success
                                    } else {
                                        Color::Warning
                                    }),
                                ),
                            )
                            .child(
                                Button::new("refresh-phone-use", "Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.status = status();
                                        this.crawl4ai_status = crawl4ai_status();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(capability(IconName::ToolWeb, "Droid-MCP - interaction layer", "Controls Android Chrome or another app for clicking, typing, scrolling, signed-in pages, and visual checks. Uses Android Accessibility; no ADB or root is required."))
                    .child(
                        h_flex().w_full().justify_end().child(
                            Button::new("enable-phone-use", if ready { "Accessibility Settings" } else { "Enable Phone Use" })
                                .on_click(|_, _, cx| cx.open_phone_use_settings()),
                        ),
                    )
                    .child(capability(IconName::TextSnippet, "Crawl4AI - understanding layer", "Optionally converts public webpages into clean Markdown or JSON before Droid-MCP interacts with them. This reduces Agent context and screenshots, but it does not share Android Chrome login state."))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(if crawl4ai_ready { IconName::Check } else { IconName::Info }).color(if crawl4ai_ready { Color::Success } else { Color::Muted }))
                            .child(Label::new(self.crawl4ai_status.clone()).size(LabelSize::Small).color(if crawl4ai_ready { Color::Success } else { Color::Muted })),
                    )
                    .child(
                        h_flex().w_full().justify_end().child(
                            Button::new(
                                "install-crawl4ai",
                                if crawl4ai_ready { "Reinstall Crawl4AI" } else { "Install Crawl4AI" },
                            )
                            .disabled(self.crawl4ai_installing)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.install_crawl4ai(window, cx);
                            })),
                        ),
                    )
                    .child(Label::new("Zdroid-B only exposes a limited native tool set. Consequential actions still require explicit user confirmation.").size(LabelSize::Small).color(Color::Muted))
                    .child(Headline::new("Office Tools").size(HeadlineSize::Small))
                    .children(OFFICE_PLUGINS.into_iter().map(|plugin| {
                        let installed = office_installed(plugin);
                        let active = office_active(plugin);
                        let installing = self.office_installing == Some(plugin.id);
                        v_flex()
                            .w_full().gap_2().p_3().rounded_md().border_1().border_color(colors.border_variant)
                            .child(h_flex().w_full().items_center().gap_2()
                                .child(Icon::new(if installed { IconName::Check } else { IconName::File }).size(IconSize::Small).color(if active { Color::Success } else { Color::Muted }))
                                .child(v_flex().min_w_0().flex_1().child(Label::new(plugin.title)).child(Label::new(plugin.detail).size(LabelSize::Small).color(Color::Muted)))
                                 .child(Switch::new(format!("office-{}-active", plugin.id), if active { ToggleState::Selected } else { ToggleState::Unselected })
                                     .disabled(!installed || installing)
                                     .on_click(cx.listener(move |this, state, _, cx| this.set_office_active(plugin, *state == ToggleState::Selected, cx))))
                            )
                             .child(h_flex().w_full().justify_between()
                                .child(Label::new(if installing { "Installing..." } else if active { "Active" } else if installed { "Inactive" } else { "Not installed" }).size(LabelSize::Small).color(if active { Color::Success } else { Color::Muted }))
                                .child(Button::new(format!("install-office-{}", plugin.id), if installed { "Installed" } else { "Install" })
                                    .disabled(installed || self.office_installing.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| this.install_office(plugin, window, cx)))))
                    }))
                    .custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Vertical)
                            .tracked_scroll_handle(&self.scroll_handle),
                        window,
                        cx,
                    ),
            )
    }
}
