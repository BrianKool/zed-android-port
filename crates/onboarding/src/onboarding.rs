use crate::multibuffer_hint::MultibufferHint;
use client::{Client, UserStore, zed_urls};
use cloud_api_types::Plan;
use db::kvp::KeyValueStore;
use fs::Fs;
use gpui::{
    Action, AnyElement, App, AppContext, AsyncWindowContext, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, Global, IntoElement, KeyContext, Render, ScrollHandle,
    SharedString, Subscription, Task, WeakEntity, Window, actions,
};
use notifications::status_toast::StatusToast;
use project::agent_server_store::AllAgentServersSettings;
use schemars::JsonSchema;
use serde::Deserialize;
use settings::{SettingsStore, VsCodeSettingsSource};
use std::sync::Arc;
use ui::{
    Divider, KeyBinding, ParentElement as _, ProgressBar, SpinnerLabel, StatefulInteractiveElement,
    Vector, VectorName, WithScrollbar as _, prelude::*, rems_from_px,
};

pub use workspace::welcome::ShowWelcome;
use workspace::welcome::WelcomePage;
use workspace::{
    AppState, DismissDecision, ModalView, Workspace, WorkspaceId,
    item::{Item, ItemEvent},
    notifications::NotifyResultExt as _,
    open_new, register_serializable_item, with_active_or_new_workspace,
};
use zed_actions::OpenOnboarding;

mod base_keymap_picker;
mod basics_page;
pub mod multibuffer_hint;
pub mod runtime_global;
mod theme_preview;

/// Imports settings from Visual Studio Code.
#[derive(Copy, Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ImportVsCodeSettings {
    #[serde(default)]
    pub skip_prompt: bool,
}

/// Imports settings from Cursor editor.
#[derive(Copy, Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ImportCursorSettings {
    #[serde(default)]
    pub skip_prompt: bool,
}

pub const FIRST_OPEN: &str = "first_open";

actions!(
    onboarding,
    [
        /// Finish the onboarding process.
        Finish,
        /// Sign in while in the onboarding flow.
        SignIn,
        /// Open the user account in zed.dev while in the onboarding flow.
        OpenAccount,
        /// Resets the welcome screen hints to their initial state.
        ResetHints
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _cx| {
        workspace
            .register_action(|_workspace, _: &ResetHints, _, cx| MultibufferHint::set_count(0, cx));
    })
    .detach();

    cx.on_action(|_: &OpenOnboarding, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            workspace
                .with_local_workspace(window, cx, |workspace, window, cx| {
                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<Onboarding>());

                    if let Some(existing) = existing {
                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let settings_page = Onboarding::new(workspace, false, cx);
                        workspace.add_item_to_active_pane(
                            Box::new(settings_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                })
                .detach();
        });
    });

    cx.on_action(|_: &ShowWelcome, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            workspace
                .with_local_workspace(window, cx, |workspace, window, cx| {
                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<WelcomePage>());

                    if let Some(existing) = existing {
                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let settings_page = cx
                            .new(|cx| WelcomePage::new(workspace.weak_handle(), false, window, cx));
                        workspace.add_item_to_active_pane(
                            Box::new(settings_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                })
                .detach();
        });
    });

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_workspace, action: &ImportVsCodeSettings, window, cx| {
            let fs = <dyn Fs>::global(cx);
            let action = *action;

            let workspace = cx.weak_entity();

            window
                .spawn(cx, async move |cx: &mut AsyncWindowContext| {
                    handle_import_vscode_settings(
                        workspace,
                        VsCodeSettingsSource::VsCode,
                        action.skip_prompt,
                        fs,
                        cx,
                    )
                    .await
                })
                .detach();
        });

        workspace.register_action(|_workspace, action: &ImportCursorSettings, window, cx| {
            let fs = <dyn Fs>::global(cx);
            let action = *action;

            let workspace = cx.weak_entity();

            window
                .spawn(cx, async move |cx: &mut AsyncWindowContext| {
                    handle_import_vscode_settings(
                        workspace,
                        VsCodeSettingsSource::Cursor,
                        action.skip_prompt,
                        fs,
                        cx,
                    )
                    .await
                })
                .detach();
        });
    })
    .detach();

    base_keymap_picker::init(cx);

    register_serializable_item::<Onboarding>(cx);
    register_serializable_item::<WelcomePage>(cx);
}

pub fn show_onboarding_view(app_state: Arc<AppState>, cx: &mut App) -> Task<anyhow::Result<()>> {
    telemetry::event!("Onboarding Page Opened");
    open_new(
        Default::default(),
        app_state,
        cx,
        |workspace, window, cx| {
            {
                workspace.close_all_docks(window, cx);
                let onboarding_page = Onboarding::new(workspace, true, cx);
                workspace.add_item_to_center(Box::new(onboarding_page.clone()), window, cx);

                window.focus(&onboarding_page.focus_handle(cx), cx);

                cx.notify();
            };
            // Android permission and runtime-picker activities can recreate the
            // workspace while onboarding is still in progress. Do not mark the
            // first-run flow complete until the user actually presses Finish.
            #[cfg(not(target_os = "android"))]
            mark_onboarding_complete(cx);
        },
    )
}

fn mark_onboarding_complete(cx: &mut App) {
    let kvp = KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        kvp.write_kvp(FIRST_OPEN.to_string(), "false".to_string())
            .await
    });
}

struct Onboarding {
    workspace: WeakEntity<Workspace>,
    initial_flow: bool,
    focus_handle: FocusHandle,
    user_store: Entity<UserStore>,
    scroll_handle: ScrollHandle,
    _settings_subscription: Subscription,
    /// Re-render trigger when the Zdroid runtime picker updates the
    /// `runtime_global::ActiveRuntime` global. Makes the
    /// `render_android_runtime_section` label reactive instead of
    /// snapshotting at first render and going stale when the user
    /// picks an adapter in the separate picker window.
    _runtime_subscription: Subscription,
}

#[cfg(target_os = "android")]
struct EssentialSetupState {
    label: SharedString,
    progress: f32,
    running: bool,
    error: Option<SharedString>,
}

#[cfg(target_os = "android")]
struct EssentialSetupModal {
    focus_handle: FocusHandle,
    state: EssentialSetupState,
}

#[cfg(target_os = "android")]
enum EssentialSetupMessage {
    Step { label: String, progress: f32 },
    Error(String),
    Done,
}

#[cfg(target_os = "android")]
fn android_essential_setup_done() -> bool {
    std::path::Path::new("/data/data/com.zdroid/files/usr/.zed/essential-packages-ok-v1").is_file()
}

#[cfg(target_os = "android")]
fn run_android_essential_setup(tx: futures::channel::mpsc::UnboundedSender<EssentialSetupMessage>) {
    use std::{
        fs,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    let prefix = "/data/data/com.zdroid/files/usr";
    let home = "/data/data/com.zdroid/files/home";
    let marker = format!("{prefix}/.zed/essential-packages-ok-v1");
    let path = format!("{prefix}/.zed/bin:{prefix}/bin:{prefix}/bin/applets:/system/bin");
    let commands = [
        (
            "Initialising package system",
            "\"$PREFIX/.zed/bin/apt\" --fix-broken install -y",
            0.05,
            0.15,
        ),
        (
            "Updating package index",
            "\"$PREFIX/.zed/bin/pkg\" update -y",
            0.15,
            0.30,
        ),
        (
            "Upgrading base packages",
            "DEBIAN_FRONTEND=noninteractive \"$PREFIX/.zed/bin/pkg\" upgrade -y -o Dpkg::Options::=\"--force-confdef\" -o Dpkg::Options::=\"--force-confold\"",
            0.30,
            0.80,
        ),
        (
            "Installing Node.js and Git",
            "\"$PREFIX/.zed/bin/pkg\" install -y nodejs-lts git",
            0.80,
            0.95,
        ),
    ];

    if android_essential_setup_done() {
        let _ = tx.unbounded_send(EssentialSetupMessage::Done);
        return;
    }

    if !std::path::Path::new(&format!("{prefix}/.zed/bin/pkg")).is_file() {
        let _ = tx.unbounded_send(EssentialSetupMessage::Error(
            "Zdroid Bootstrap is not installed yet. Install Bootstrap in Android Runtime first."
                .to_string(),
        ));
        return;
    }

    for (label, command, start_progress, end_progress) in commands.iter() {
        let _ = tx.unbounded_send(EssentialSetupMessage::Step {
            label: (*label).to_string(),
            progress: *start_progress,
        });

        let mut child = match Command::new("/system/bin/sh")
            .arg("-c")
            .arg(command)
            .env("PREFIX", prefix)
            .env("HOME", home)
            .env("PATH", &path)
            .env("LD_LIBRARY_PATH", format!("{prefix}/lib"))
            .current_dir(home)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = tx.unbounded_send(EssentialSetupMessage::Error(format!(
                    "{label} failed to start: {error}"
                )));
                return;
            }
        };

        let started_at = Instant::now();
        let mut heartbeat = 0_u32;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    thread::sleep(Duration::from_secs(8));
                    heartbeat += 1;
                    let elapsed = started_at.elapsed().as_secs();
                    let progress_span = end_progress - start_progress;
                    let heartbeat_progress = (*start_progress
                        + progress_span * 0.85 * (heartbeat as f32 / 30.0))
                        .min(*end_progress - 0.01);
                    let _ = tx.unbounded_send(EssentialSetupMessage::Step {
                        label: format!("{label}... still working ({elapsed}s)"),
                        progress: heartbeat_progress,
                    });
                }
                Err(error) => break Err(error),
            }
        };

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let _ = tx.unbounded_send(EssentialSetupMessage::Error(format!(
                    "{label} failed with exit code {}",
                    status.code().unwrap_or(-1)
                )));
                return;
            }
            Err(error) => {
                let _ = tx.unbounded_send(EssentialSetupMessage::Error(format!(
                    "{label} failed while waiting: {error}"
                )));
                return;
            }
        }

        let _ = tx.unbounded_send(EssentialSetupMessage::Step {
            label: format!("{label} completed"),
            progress: *end_progress,
        });
    }

    let _ = tx.unbounded_send(EssentialSetupMessage::Step {
        label: "Finalising essential packages".to_string(),
        progress: 1.0,
    });
    if let Some(parent) = std::path::Path::new(&marker).parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&marker, b"ok\n") {
        Ok(()) => {
            let _ = tx.unbounded_send(EssentialSetupMessage::Done);
        }
        Err(error) => {
            let _ = tx.unbounded_send(EssentialSetupMessage::Error(format!(
                "Essential packages installed, but setup marker could not be saved: {error}"
            )));
        }
    }
}

#[cfg(target_os = "android")]
impl EssentialSetupModal {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut modal = Self {
            focus_handle: cx.focus_handle(),
            state: EssentialSetupState {
                label: "Initialising essential packages".into(),
                progress: 0.0,
                running: false,
                error: None,
            },
        };
        modal.start(cx);
        modal
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        use futures::StreamExt as _;

        if self.state.running {
            return;
        }

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<EssentialSetupMessage>();
        self.state = EssentialSetupState {
            label: "Initialising essential packages".into(),
            progress: 0.0,
            running: true,
            error: None,
        };
        cx.start_background_task(
            "zdroid-essential-packages",
            "Initialising essential packages",
        );
        cx.background_executor()
            .spawn(async move { run_android_essential_setup(tx) })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let done = matches!(message, EssentialSetupMessage::Done);
                let _ = this.update(cx, |this, cx| {
                    match message {
                        EssentialSetupMessage::Step { label, progress } => {
                            cx.start_background_task("zdroid-essential-packages", &label);
                            this.state = EssentialSetupState {
                                label: label.into(),
                                progress,
                                running: true,
                                error: None,
                            };
                        }
                        EssentialSetupMessage::Error(error) => {
                            cx.finish_background_task(
                                "zdroid-essential-packages",
                                "Initialising essential packages",
                                false,
                            );
                            this.state = EssentialSetupState {
                                label: "Essential package setup failed".into(),
                                progress: 0.0,
                                running: false,
                                error: Some(error.into()),
                            };
                        }
                        EssentialSetupMessage::Done => {
                            cx.finish_background_task(
                                "zdroid-essential-packages",
                                "Initialising essential packages",
                                true,
                            );
                            this.state = EssentialSetupState {
                                label: "Initialisation complete".into(),
                                progress: 1.0,
                                running: false,
                                error: None,
                            };
                        }
                    }
                    cx.notify();
                    if done {
                        cx.emit(DismissEvent);
                    }
                });
            }
        })
        .detach();
    }
}

#[cfg(target_os = "android")]
impl Render for EssentialSetupModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_upgrade = self
            .state
            .label
            .as_ref()
            .starts_with("Upgrading base packages");

        v_flex()
            .id("zdroid-essential-setup-dialog")
            .w(rems_from_px(560.0))
            .max_w_full()
            .max_h_full()
            .min_w_0()
            .gap_3()
            .p_4()
            .track_focus(&self.focus_handle)
            .child(Headline::new("Setting up Zdroid-B").size(HeadlineSize::Small))
            .child(
                Label::new(
                    "Installing the essential Linux packages used by the terminal and AI agents.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(
                Label::new(
                    "Estimated time: around 10 minutes. Setup continues in the background, so you can leave Zdroid-B and return later.",
                )
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .when(self.state.running, |this| {
                                this.child(SpinnerLabel::dots_variant().size(LabelSize::Small))
                            })
                            .child(
                                Label::new(self.state.label.clone())
                                    .size(LabelSize::Small)
                                    .truncate(),
                            ),
                    )
                    .child(
                        Label::new(format!("{:.0}%", self.state.progress * 100.0))
                            .flex_shrink_0()
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .child(ProgressBar::new(
                "zdroid-essential-setup-progress",
                self.state.progress * 100.0,
                100.0,
                cx,
            ))
            .when(is_upgrade && self.state.error.is_none(), |this| {
                this.child(
                    Label::new(
                        "The package upgrade is usually the longest step. The notification will keep showing its current status.",
                    )
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
                )
            })
            .when_some(self.state.error.clone(), |this, error| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            Label::new(error)
                                .size(LabelSize::XSmall)
                                .color(Color::Error),
                        )
                        .child(
                            Button::new("retry-essential-setup", "Retry")
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                        ),
                )
            })
    }
}

#[cfg(target_os = "android")]
impl ModalView for EssentialSetupModal {
    fn on_before_dismiss(&mut self, _: &mut Window, _: &mut Context<Self>) -> DismissDecision {
        DismissDecision::Dismiss(!self.state.running)
    }

    fn show_close_button(&self) -> bool {
        !self.state.running
    }
}

#[cfg(target_os = "android")]
impl Focusable for EssentialSetupModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(target_os = "android")]
impl EventEmitter<DismissEvent> for EssentialSetupModal {}

impl Onboarding {
    fn new(workspace: &Workspace, initial_flow: bool, cx: &mut App) -> Entity<Self> {
        let font_family_cache = theme::FontFamilyCache::global(cx);

        let installed_agents = cx
            .global::<SettingsStore>()
            .get::<AllAgentServersSettings>(None)
            .clone();
        let client = Client::global(cx);
        let status = *client.status().borrow();
        let plan = workspace.user_store().read(cx).plan();
        let zed_agent_state = if status.is_signed_out()
            || matches!(
                status,
                client::Status::AuthenticationError | client::Status::ConnectionError
            ) {
            "signed_out"
        } else if status.is_signing_in() {
            "signing_in"
        } else {
            match plan {
                Some(Plan::ZedPro) => "pro",
                Some(Plan::ZedProTrial) => "trial",
                Some(Plan::ZedBusiness) => "business",
                Some(Plan::ZedVip) => "vip",
                Some(Plan::ZedStudent) => "student",
                Some(Plan::ZedFree) | None => "free",
            }
        };
        let agents_installed = basics_page::FEATURED_AGENT_IDS
            .iter()
            .filter(|id| installed_agents.contains_key(**id))
            .copied()
            .collect::<Vec<_>>();
        telemetry::event!(
            "Welcome Agent Setup Viewed",
            zed_agent = zed_agent_state,
            agents_installed = agents_installed,
        );

        cx.new(|cx| {
            cx.spawn(async move |this, cx| {
                font_family_cache.prefetch(cx).await;
                this.update(cx, |_, cx| {
                    cx.notify();
                })
            })
            .detach();

            Self {
                workspace: workspace.weak_handle(),
                initial_flow,
                focus_handle: cx.focus_handle(),
                scroll_handle: ScrollHandle::new(),
                user_store: workspace.user_store().clone(),
                _settings_subscription: cx
                    .observe_global::<SettingsStore>(move |_, cx| cx.notify()),
                _runtime_subscription: cx.observe_global::<crate::runtime_global::ActiveRuntime>(
                    move |_, cx| cx.notify(),
                ),
            }
        })
    }

    #[cfg(not(target_os = "android"))]
    fn handle_finish(&mut self, _: &Finish, _: &mut Window, cx: &mut Context<Self>) {
        telemetry::event!("Finish Setup");
        go_to_welcome_page(cx);
    }

    #[cfg(target_os = "android")]
    fn handle_finish(&mut self, _: &Finish, window: &mut Window, cx: &mut Context<Self>) {
        telemetry::event!("Finish Setup");

        if self.initial_flow {
            mark_onboarding_complete(cx);
        }

        if android_essential_setup_done() {
            go_to_welcome_page(cx);
            return;
        }

        let workspace = self.workspace.clone();
        go_to_welcome_page(cx);
        if let Some(workspace) = workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |_, cx| EssentialSetupModal::new(cx));
            });
        }
    }

    fn handle_sign_in(&mut self, _: &SignIn, window: &mut Window, cx: &mut Context<Self>) {
        let client = Client::global(cx);
        let workspace = self.workspace.clone();

        window
            .spawn(cx, async move |mut cx| {
                client
                    .sign_in_with_optional_connect(true, &cx)
                    .await
                    .notify_workspace_async_err(workspace, &mut cx);
            })
            .detach();
    }

    fn handle_open_account(_: &OpenAccount, _: &mut Window, cx: &mut App) {
        cx.open_url(&zed_urls::account_url(cx))
    }

    fn render_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        crate::basics_page::render_basics_page(&self.user_store, cx).into_any_element()
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let root = div()
            .image_cache(gpui::retain_all("onboarding-page"))
            .key_context({
                let mut ctx = KeyContext::new_with_defaults();
                ctx.add("Onboarding");
                ctx.add("menu");
                ctx
            })
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background);

        let waiting_for_initial_runtime = cfg!(target_os = "android")
            && self.initial_flow
            && cx
                .try_global::<crate::runtime_global::ActiveRuntime>()
                .is_none_or(|runtime| runtime.current.is_none());
        if waiting_for_initial_runtime {
            return root.into_any_element();
        }

        root.on_action(cx.listener(Self::handle_finish))
            .on_action(cx.listener(Self::handle_sign_in))
            .on_action(Self::handle_open_account)
            .on_action(cx.listener(|_, _: &menu::SelectNext, window, cx| {
                window.focus_next(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &menu::SelectPrevious, window, cx| {
                window.focus_prev(cx);
                cx.notify();
            }))
            .vertical_scrollbar_for(&self.scroll_handle, window, cx)
            .child(
                div()
                    .id("page-content")
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .min_w_0()
                            .max_w(rems_from_px(780.))
                            .w_full()
                            .mx_auto()
                            .p_12()
                            .gap_6()
                            .child({
                                let compact = window.viewport_size().width < px(560.0);
                                let finish_button = Button::new("finish_setup", "Finish Setup")
                                    .style(ButtonStyle::Filled)
                                    .size(ButtonSize::Medium)
                                    .when(compact, |button| button.full_width())
                                    .when(!compact, |button| button.width(rems_from_px(200.)))
                                    .key_binding(KeyBinding::for_action_in(
                                        &Finish,
                                        &self.focus_handle,
                                        cx,
                                    ))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Finish.boxed_clone(), cx);
                                    });

                                h_flex()
                                    .w_full()
                                    .gap_4()
                                    .justify_between()
                                    .when(compact, |this| this.flex_col().items_start().gap_3())
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .child(Vector::square(VectorName::ZedLogo, rems(2.5)))
                                            .child(
                                                v_flex()
                                                    .child(
                                                        Headline::new("Welcome to Zdroid")
                                                            .size(HeadlineSize::Small),
                                                    )
                                                    .child(
                                                        Label::new("The editor for what's next")
                                                            .color(Color::Muted)
                                                            .size(LabelSize::Small)
                                                            .italic(),
                                                    ),
                                            ),
                                    )
                                    .child(finish_button)
                            })
                            .child(Divider::horizontal().color(ui::DividerColor::BorderVariant))
                            .child(self.render_page(cx)),
                    )
                    .track_scroll(&self.scroll_handle),
            )
            .into_any_element()
    }
}

impl EventEmitter<ItemEvent> for Onboarding {}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for Onboarding {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Onboarding".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Onboarding Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn full_workspace(&self) -> bool {
        cfg!(target_os = "android") && self.initial_flow
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>> {
        Task::ready(Some(cx.new(|cx| Onboarding {
            workspace: self.workspace.clone(),
            initial_flow: false,
            user_store: self.user_store.clone(),
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _settings_subscription: cx.observe_global::<SettingsStore>(move |_, cx| cx.notify()),
            _runtime_subscription:
                cx.observe_global::<crate::runtime_global::ActiveRuntime>(move |_, cx| cx.notify()),
        })))
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        f(*event)
    }
}

fn go_to_welcome_page(cx: &mut App) {
    with_active_or_new_workspace(cx, |workspace, window, cx| {
        let Some((onboarding_id, onboarding_idx)) = workspace
            .active_pane()
            .read(cx)
            .items()
            .enumerate()
            .find_map(|(idx, item)| {
                let _ = item.downcast::<Onboarding>()?;
                Some((item.item_id(), idx))
            })
        else {
            return;
        };

        workspace.active_pane().update(cx, |pane, cx| {
            // Get the index here to get around the borrow checker
            let idx = pane.items().enumerate().find_map(|(idx, item)| {
                let _ = item.downcast::<WelcomePage>()?;
                Some(idx)
            });

            if let Some(idx) = idx {
                pane.activate_item(idx, true, true, window, cx);
            } else {
                let item = Box::new(
                    cx.new(|cx| WelcomePage::new(workspace.weak_handle(), false, window, cx)),
                );
                pane.add_item(item, true, true, Some(onboarding_idx), window, cx);
            }

            pane.remove_item(onboarding_id, false, false, window, cx);
        });
    });
}

pub async fn handle_import_vscode_settings(
    workspace: WeakEntity<Workspace>,
    source: VsCodeSettingsSource,
    skip_prompt: bool,
    fs: Arc<dyn Fs>,
    cx: &mut AsyncWindowContext,
) {
    use util::truncate_and_remove_front;

    let vscode_settings =
        match settings::VsCodeSettings::load_user_settings(source, fs.clone()).await {
            Ok(vscode_settings) => vscode_settings,
            Err(err) => {
                zlog::error!("{err:?}");
                let _ = cx.prompt(
                    gpui::PromptLevel::Info,
                    &format!("Could not find or load a {source} settings file"),
                    None,
                    &["OK"],
                );
                return;
            }
        };

    if !skip_prompt {
        let prompt = cx.prompt(
            gpui::PromptLevel::Warning,
            &format!(
                "Importing {} settings may overwrite your existing settings. \
                Will import settings from {}",
                vscode_settings.source,
                truncate_and_remove_front(&vscode_settings.path.to_string_lossy(), 128),
            ),
            None,
            &["Import", "Cancel"],
        );
        let result = cx.spawn(async move |_| prompt.await.ok()).await;
        if result != Some(0) {
            return;
        }
    };

    let Ok(result_channel) = cx.update(|_, cx| {
        let source = vscode_settings.source;
        let path = vscode_settings.path.clone();
        let result_channel = cx
            .global::<SettingsStore>()
            .import_vscode_settings(fs, vscode_settings);
        zlog::info!("Imported {source} settings from {}", path.display());
        result_channel
    }) else {
        return;
    };

    let result = result_channel.await;
    workspace
        .update_in(cx, |workspace, _, cx| match result {
            Ok(_) => {
                let confirmation_toast = StatusToast::new(
                    format!("Your {} settings were successfully imported.", source),
                    cx,
                    |this, _| {
                        this.icon(
                            Icon::new(IconName::Check)
                                .size(IconSize::Small)
                                .color(Color::Success),
                        )
                        .dismiss_button(true)
                    },
                );
                SettingsImportState::update(cx, |state, _| match source {
                    VsCodeSettingsSource::VsCode => {
                        state.vscode = true;
                    }
                    VsCodeSettingsSource::Cursor => {
                        state.cursor = true;
                    }
                });
                workspace.toggle_status_toast(confirmation_toast, cx);
            }
            Err(_) => {
                let error_toast = StatusToast::new(
                    "Failed to import settings. See log for details",
                    cx,
                    |this, _| {
                        this.icon(
                            Icon::new(IconName::Close)
                                .size(IconSize::Small)
                                .color(Color::Error),
                        )
                        .action("Open Log", |window, cx| {
                            window.dispatch_action(workspace::OpenLog.boxed_clone(), cx)
                        })
                        .dismiss_button(true)
                    },
                );
                workspace.toggle_status_toast(error_toast, cx);
            }
        })
        .ok();
}

#[derive(Default, Copy, Clone)]
pub struct SettingsImportState {
    pub cursor: bool,
    pub vscode: bool,
}

impl Global for SettingsImportState {}

impl SettingsImportState {
    pub fn global(cx: &App) -> Self {
        cx.try_global().cloned().unwrap_or_default()
    }
    pub fn update<R>(cx: &mut App, f: impl FnOnce(&mut Self, &mut App) -> R) -> R {
        cx.update_default_global(f)
    }
}

impl workspace::SerializableItem for Onboarding {
    fn serialized_item_kind() -> &'static str {
        "OnboardingPage"
    }

    fn cleanup(
        workspace_id: workspace::WorkspaceId,
        alive_items: Vec<workspace::ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> gpui::Task<gpui::Result<()>> {
        workspace::delete_unloaded_items(
            alive_items,
            workspace_id,
            "onboarding_pages",
            &persistence::OnboardingPagesDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        _project: Entity<project::Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: workspace::WorkspaceId,
        item_id: workspace::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::Task<gpui::Result<Entity<Self>>> {
        let db = persistence::OnboardingPagesDb::global(cx);
        window.spawn(cx, async move |cx| {
            if let Some(_) = db.get_onboarding_page(item_id, workspace_id)? {
                workspace.update(cx, |workspace, cx| Onboarding::new(workspace, false, cx))
            } else {
                Err(anyhow::anyhow!("No onboarding page to deserialize"))
            }
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: workspace::ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut ui::Context<Self>,
    ) -> Option<gpui::Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;

        let db = persistence::OnboardingPagesDb::global(cx);
        Some(
            cx.background_spawn(
                async move { db.save_onboarding_page(item_id, workspace_id).await },
            ),
        )
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        event == &ItemEvent::UpdateTab
    }
}

mod persistence {
    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };
    use workspace::WorkspaceDb;

    pub struct OnboardingPagesDb(ThreadSafeConnection);

    impl Domain for OnboardingPagesDb {
        const NAME: &str = stringify!(OnboardingPagesDb);

        const MIGRATIONS: &[&str] = &[
            sql!(
                        CREATE TABLE onboarding_pages (
                            workspace_id INTEGER,
                            item_id INTEGER UNIQUE,
                            page_number INTEGER,

                            PRIMARY KEY(workspace_id, item_id),
                            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                            ON DELETE CASCADE
                        ) STRICT;
            ),
            sql!(
                        CREATE TABLE onboarding_pages_2 (
                            workspace_id INTEGER,
                            item_id INTEGER UNIQUE,

                            PRIMARY KEY(workspace_id, item_id),
                            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                            ON DELETE CASCADE
                        ) STRICT;
                        INSERT INTO onboarding_pages_2 SELECT workspace_id, item_id FROM onboarding_pages;
                        DROP TABLE onboarding_pages;
                        ALTER TABLE onboarding_pages_2 RENAME TO onboarding_pages;
            ),
        ];
    }

    db::static_connection!(OnboardingPagesDb, [WorkspaceDb]);

    impl OnboardingPagesDb {
        query! {
            pub async fn save_onboarding_page(
                item_id: workspace::ItemId,
                workspace_id: workspace::WorkspaceId
            ) -> Result<()> {
                INSERT OR REPLACE INTO onboarding_pages(item_id, workspace_id)
                VALUES (?, ?)
            }
        }

        query! {
            pub fn get_onboarding_page(
                item_id: workspace::ItemId,
                workspace_id: workspace::WorkspaceId
            ) -> Result<Option<workspace::ItemId>> {
                SELECT item_id
                FROM onboarding_pages
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}
