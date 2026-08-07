//! Runtime adapter picker â€” lets the user pick which userland Zdroid
//! routes its spawns through (chroot, bootstrap, external Termux).
//!
//! Surfaces as a centered modal triggered by the `zdroid: pick runtime`
//! action. Mirrors Zed's welcome-page card aesthetic.
//!
//! v1 scope (this commit):
//!   - Render three cards with name, tagline, live health snapshot.
//!   - Select button per card. Click logs the choice + dismisses.
//!     Persistence to `runtime.toml` lands once we've nailed down
//!     where Zdroid stores adapter config (likely `$PREFIX/etc/zd-runtime.toml`)
//!     and added the file-write helper.
//!
//! Future scope (queued in tasks #33/#34):
//!   - Install / Uninstall buttons backed by adapter `install()` paths.
//!   - "Restart Zdroid" prompt after a switch.
//!   - First-launch auto-open when no `runtime.toml` exists yet.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Render, ScrollHandle, StatefulInteractiveElement, Tiling, Window, actions,
    prelude::*,
};
use platform_title_bar::PlatformTitleBar;
use theme::ActiveTheme;
use ui::{
    Button, Chip, Clickable, Color, Disableable, FixedWidth, FluentBuilder, Headline, HeadlineSize,
    Icon, IconName, IconSize, Label, LabelCommon, LabelSize, ParentElement, Styled, WithScrollbar,
    div, h_flex, v_flex,
};
use util::ResultExt as _;
use workspace::{ModalView, MultiWorkspace, Workspace, client_side_decorations};
use zdroid_runtime::{
    HealthStatus, RuntimeId, RuntimeProvider, adapters,
    adapters::chroot::SPAWND_RELEASE_URL,
    config::{BootstrapConfig, ChrootConfig, ExternalTermuxConfig, RuntimeFile},
    health::ProgressSink,
};

/// Bridges the sync `ProgressSink` trait (called from the background
/// install thread) into an async channel the foreground UI poller
/// reads. `step`, `progress`, and `warn` are forwarded as status strings.
struct ChannelProgressSink {
    tx: futures::channel::mpsc::UnboundedSender<String>,
    last_percent: Option<u64>,
}

impl ProgressSink for ChannelProgressSink {
    fn step(&mut self, label: &str) {
        log::info!("zdroid_runtime_picker: step: {}", label);
        let _ = self.tx.unbounded_send(label.to_string());
    }
    fn progress(&mut self, done: u64, total: u64) {
        if total > 0 {
            let pct = done.saturating_mul(100) / total;
            if self.last_percent == Some(pct) {
                return;
            }
            self.last_percent = Some(pct);
            let _ = self
                .tx
                .unbounded_send(format!("Downloading bootstrap {pct}%"));
        }
    }
    fn warn(&mut self, message: &str) {
        log::warn!("zdroid_runtime_picker: warn: {}", message);
        let _ = self.tx.unbounded_send(format!("warning: {message}"));
    }
}

/// Where Zdroid stores the active-adapter selection. Lives inside
/// `$PREFIX/etc/` so the bootstrap-extraction step doesn't clobber it
/// (extraction doesn't touch `etc/`), and so it persists across
/// editor APK updates the same way other user state does.
const RUNTIME_TOML_PATH: &str = "/data/data/com.zdroid/files/usr/etc/zd-runtime.toml";
static FIRST_RUNTIME_PICKER_OPENED: AtomicBool = AtomicBool::new(false);

actions!(
    zdroid_runtime,
    [
        /// Open the runtime adapter picker modal.
        PickRuntime,
    ]
);

/// Register the `zdroid_runtime::PickRuntime` action. Called from
/// `lib.rs::android_main` at workspace init. Once registered, the
/// action can be triggered from anywhere via `cx.build_action(...)` +
/// `window.dispatch_action(...)`. Three current entry points:
///
///   - Command palette (`zdroid: pick runtime`).
///   - Settings â†’ "Android Runtime" â†’ "Open picker".
///   - Onboarding basics page â†’ "Set up Android runtime" button.
///
/// The handler opens the picker as a workspace modal so phone and DeX
/// both keep it attached to MainActivity and can dismiss it predictably.
pub fn register(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, cx: &mut Context<Workspace>| {
            workspace.register_action(handle_pick_runtime);

            if cfg!(target_os = "android")
                && !std::path::Path::new(RUNTIME_TOML_PATH).exists()
                && !FIRST_RUNTIME_PICKER_OPENED.swap(true, Ordering::AcqRel)
            {
                cx.spawn(async move |_workspace, cx| {
                    while !gpui_android::storage::initial_permissions_settled() {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(100))
                            .await;
                    }
                    log::info!(
                        "zdroid_runtime_picker: initial permissions settled; opening first-time runtime setup"
                    );
                    cx.update(open_runtime_picker)
                })
                .detach();
            }
        },
    )
    .detach();
}

fn handle_pick_runtime(
    workspace: &mut Workspace,
    _: &PickRuntime,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.toggle_modal(window, cx, |_, cx| RuntimePicker::new(cx));
}

/// Show the runtime picker in the active workspace. Public so any callsite
/// (settings page on_click, onboarding button on_click, lib.rs first-
/// launch hook if we ever add one back) can open the same picker with
/// the same parameters. Dedupes against an already-open instance so
/// repeated taps don't pile up windows.
///
/// `_window` is unused but matches the on_click signature gpui passes
/// to settings_ui's ActionLink so we don't need a wrapper closure at
/// every call site.
pub fn open_runtime_picker_window(_window: &mut Window, cx: &mut App) {
    open_runtime_picker(cx);
}

fn open_runtime_picker(cx: &mut App) {
    let workspace_window = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<MultiWorkspace>());

    let Some(workspace_window) = workspace_window else {
        log::error!("zdroid_runtime_picker: no active workspace window");
        return;
    };

    workspace_window
        .update(cx, |multi_workspace, window, cx| {
            multi_workspace.toggle_modal(window, cx, |_, cx| RuntimePicker::new(cx));
        })
        .log_err();
}

struct AdapterEntry {
    id: RuntimeId,
    tagline: &'static str,
    health: HealthStatus,
}

pub struct RuntimePicker {
    title_bar: Option<Entity<PlatformTitleBar>>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    entries: Vec<AdapterEntry>,
    /// The currently active adapter (from disk). Marked with a
    /// "Current" badge in the UI; `Select` is a no-op if the user
    /// picks the same one.
    current: Option<RuntimeId>,
    /// Live status string while a bootstrap install is running.
    /// `None` when no install is in flight. The background task
    /// pushes updates via a channel; the foreground poller writes
    /// them here + calls `cx.notify()` so the install button's
    /// label re-renders without the user having to interact.
    install_status: Option<String>,
    /// Last bootstrap install error. Kept visible after the task exits.
    install_error: Option<String>,
    /// True once an adapter selection has been saved to
    /// `runtime.toml` and the user needs to fully close and reopen
    /// the app for the change to take effect. Drives the inline
    /// banner at the top of the picker. We don't attempt an in-app
    /// restart (canonical Android patterns interact poorly with
    /// Background Activity Launch rules and per-OEM task lifecycle
    /// policies); the user closes via Recents and reopens from the
    /// launcher.
    restart_required: bool,
}

impl RuntimePicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let title_bar = if !cfg!(any(target_os = "macos", target_os = "android")) {
            Some(cx.new(|cx| PlatformTitleBar::new("runtime-picker-title-bar", cx)))
        } else {
            None
        };
        Self {
            title_bar,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            entries: build_entries(),
            current: detect_current(),
            install_status: None,
            install_error: None,
            restart_required: false,
        }
    }

    /// Trigger an async bootstrap install. Drops the user into a
    /// "downloading + extracting" state on the Bootstrap card while
    /// the background task pulls the latest release zip from GitHub
    /// and extracts to `$PREFIX`. Refreshes adapter health on
    /// completion so the card flips from NotInstalled â†’ Healthy
    /// without the user having to re-open the picker.
    fn install_bootstrap(&mut self, cx: &mut Context<Self>) {
        if self.install_status.is_some() {
            return; // already in progress
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        self.install_status = Some("Starting install".into());
        self.install_error = None;
        cx.notify();

        // Background: run the actual install. Blocks on ureq +
        // zip extract. ProgressSink pushes status strings into the
        // channel; the foreground poller below picks them up.
        cx.background_executor()
            .spawn(async move {
                let config = default_bootstrap_config();
                let adapter = match adapters::bootstrap::BootstrapAdapter::new(config) {
                    Ok(a) => a,
                    Err(err) => {
                        log::error!("zdroid_runtime_picker: BootstrapAdapter::new failed: {err:#}");
                        let _ = tx.unbounded_send(format!("ERROR: {err:#}"));
                        return;
                    }
                };
                let mut sink = ChannelProgressSink {
                    tx: tx.clone(),
                    last_percent: None,
                };
                match adapter.install(&mut sink) {
                    Ok(()) => {
                        if let Err(err) = super::ensure_agent_cli_launchers() {
                            log::error!(
                                "zdroid_runtime_picker: Agent launcher repair failed: {err:#}"
                            );
                            let _ = tx.unbounded_send(format!(
                                "ERROR: Bootstrap installed, but Agent launchers could not be created: {err:#}"
                            ));
                        }
                    }
                    Err(err) => {
                        log::error!(
                            "zdroid_runtime_picker: BootstrapAdapter::install failed: {err:#}"
                        );
                        let _ = tx.unbounded_send(format!("ERROR: {err:#}"));
                    }
                }
                // tx + sink drop here â†’ channel closes â†’ foreground exits.
            })
            .detach();

        // Foreground poll: drain the channel, write each message into
        // self.install_status + cx.notify so the card label updates.
        // When the channel closes (background task done), refresh the
        // adapter entries and clear the in-progress state.
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(error) = msg.strip_prefix("ERROR: ") {
                        this.install_error = Some(error.to_string());
                    } else {
                        this.install_status = Some(msg);
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |this, cx| {
                this.install_status = None;
                this.entries = build_entries();
                let bootstrap_ready = this.entries.iter().any(|entry| {
                    entry.id == RuntimeId::Bootstrap
                        && matches!(entry.health, HealthStatus::Healthy)
                });
                if this.install_error.is_none() && this.current.is_none() && bootstrap_ready {
                    let path = std::path::PathBuf::from(RUNTIME_TOML_PATH);
                    match RuntimeFile::with_defaults(RuntimeId::Bootstrap).save(&path) {
                        Ok(()) => {
                            this.current = Some(RuntimeId::Bootstrap);
                            cx.set_global(onboarding::runtime_global::ActiveRuntime {
                                current: Some(RuntimeId::Bootstrap),
                            });
                            log::info!(
                                "zdroid_runtime_picker: first Bootstrap install selected without restart"
                            );
                        }
                        Err(err) => {
                            this.install_error = Some(format!(
                                "Bootstrap installed, but its runtime selection could not be saved: {err:#}"
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: RuntimeId, _window: &mut Window, cx: &mut Context<Self>) {
        if id == RuntimeId::ExternalTermux {
            log::warn!(
                "zdroid_runtime_picker: refusing External Termux selection until its stdio bridge is implemented"
            );
            return;
        }
        if Some(id) == self.current {
            log::info!("zdroid_runtime_picker: {:?} already active; no-op", id);
            return;
        }
        let first_selection = self.current.is_none();

        let path = std::path::PathBuf::from(RUNTIME_TOML_PATH);
        let file = RuntimeFile::with_defaults(id);
        match file.save(&path) {
            Ok(()) => {
                log::info!(
                    "zdroid_runtime_picker: selected {:?} -> wrote {}; restart Zdroid to apply",
                    id,
                    path.display()
                );
                self.current = Some(id);
                // Update the gpui Global so any entity observing it
                // (e.g. the onboarding page's "Current: <adapter>"
                // label) re-renders without waiting for an app
                // restart. set_global pushes
                // NotifyGlobalObservers which fans out to every
                // registered observer.
                cx.set_global(onboarding::runtime_global::ActiveRuntime { current: Some(id) });
                cx.notify();

                // Surface the close-and-reopen requirement inline,
                // styled with the picker's own theme â€” see Render
                // for the banner. Window-level `window.prompt` is
                // the native Android AlertDialog which looks out of
                // place against the editor's chrome. We deliberately
                // don't attempt an in-app restart either; the
                // canonical approaches (AlarmManager + PendingIntent,
                // startActivity + delayed kill, ActivityManager
                // appTasks sweep) all interact poorly with Android's
                // evolving Background Activity Launch rules and per-
                // OEM task lifecycle policies.
                self.restart_required = !first_selection || id != RuntimeId::Bootstrap;
            }
            Err(err) => {
                log::error!(
                    "zdroid_runtime_picker: failed to write {}: {:#}",
                    path.display(),
                    err
                );
            }
        }
    }
}

impl Focusable for RuntimePicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for RuntimePicker {}

impl ModalView for RuntimePicker {}

impl Render for RuntimePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Copy out the few theme colors we need so the immutable borrow
        // of `cx.theme()` doesn't conflict with the mutable borrow we
        // need later for `render_card` and `client_side_decorations`.
        let bg = cx.theme().colors().editor_background;
        let text = cx.theme().colors().text;
        let compact = cfg!(target_os = "android") && window.viewport_size().width.as_f32() < 520.0;

        let cards: Vec<AnyElement> = self
            .entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                render_card(
                    idx,
                    entry,
                    self.current,
                    self.install_status.as_deref(),
                    self.install_error.as_deref(),
                    compact,
                    cx,
                )
            })
            .collect();

        let banner = self.restart_required.then(|| {
            let border = cx.theme().colors().border;
            let banner_bg = cx.theme().colors().element_background;
            let warning = cx.theme().status().warning;
            h_flex()
                .gap_3()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .bg(banner_bg)
                .items_start()
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Custom(warning)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            Label::new("Restart to apply")
                                .size(LabelSize::Default)
                                .color(Color::Default),
                        )
                        .child(
                            Label::new("Runtime adapter switched.")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
        });

        // Keep the viewport at the available window height and let this inner
        // column retain its natural height. If the scroll node itself is a
        // flex column, its cards shrink to fit and GPUI sees no overflow.
        let content = v_flex()
            .w_full()
            .p_6()
            .gap_4()
            .when(compact, |this| this.p_3().gap_3())
            .bg(bg)
            .when(cfg!(target_os = "macos"), |this| this.pt_10())
            .child(
                v_flex()
                    .gap_1()
                    .child(Headline::new("Pick your runtime").size(HeadlineSize::Medium))
                    .child(
                        Label::new(
                            "Where Zdroid runs your tools: LSPs, git, formatters, terminal. \
                             Switch any time from Settings.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .when_some(banner, |this, banner| this.child(banner))
            .child(v_flex().w_full().gap_3().children(cards));

        let scroll_viewport = div()
            .id("runtime-picker-scroll")
            .key_context("RuntimePicker")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(bg)
            .child(content);

        client_side_decorations(
            v_flex()
                .size_full()
                .text_color(text)
                .children(self.title_bar.clone())
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(scroll_viewport)
                        .vertical_scrollbar_for(&self.scroll_handle, window, cx),
                ),
            window,
            cx,
            Tiling::default(),
        )
    }
}

fn render_card(
    idx: usize,
    entry: &AdapterEntry,
    current: Option<RuntimeId>,
    install_status: Option<&str>,
    install_error: Option<&str>,
    compact: bool,
    cx: &mut Context<RuntimePicker>,
) -> AnyElement {
    let theme_colors = cx.theme().colors();
    let id = entry.id;
    let is_current = current == Some(id);
    let name = id.display_name();
    let tagline = entry.tagline;

    let (dot_color, dot_label): (Color, &'static str) = match &entry.health {
        HealthStatus::Healthy => (Color::Success, "Ready"),
        HealthStatus::NotInstalled { .. } => (Color::Muted, "Not installed"),
        HealthStatus::Misconfigured { .. } => (Color::Warning, "Needs attention"),
        HealthStatus::Failed { .. } => (Color::Error, "Failed"),
    };

    let detail = match &entry.health {
        HealthStatus::Healthy => None,
        HealthStatus::NotInstalled { hint } => Some(hint.clone()),
        HealthStatus::Misconfigured { reason } => Some(reason.clone()),
        HealthStatus::Failed { error } => Some(error.clone()),
    };
    let detail = if id == RuntimeId::Bootstrap {
        install_error
            .map(|error| format!("Install failed: {error}"))
            .or(detail)
    } else {
        detail
    };
    let detail_color = if id == RuntimeId::Bootstrap && install_error.is_some() {
        Color::Error
    } else {
        Color::Muted
    };

    h_flex()
        .id(("adapter-card", idx))
        .gap_4()
        .when(compact, |this| this.flex_col().items_start())
        .p_4()
        .when(compact, |this| this.p_3().gap_3())
        .w_full()
        .border_1()
        .border_color(if is_current {
            theme_colors.border_focused
        } else {
            theme_colors.border_variant
        })
        .rounded_md()
        // Allow inner flex children to shrink below their content
        // width (CSS `min-width: 0` equivalent) â€” without this the
        // long tagline labels push the layout past the modal's edge.
        .min_w_0()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Server).size(IconSize::Small))
                        .child(Headline::new(name).size(HeadlineSize::XSmall))
                        .when(is_current, |row| {
                            // Use ui::Chip â€” the canonical Zed badge
                            // primitive (same one agent_ui uses for
                            // "Latest" tags etc.). Matches the rest of
                            // the editor's design language out of the
                            // box.
                            row.child(
                                Chip::new("Active")
                                    .icon(IconName::Check)
                                    .label_color(Color::Accent),
                            )
                        }),
                )
                .when(id == RuntimeId::Bootstrap, |this| {
                    this.child(
                        Label::new("Recommended - required for AI agents")
                            .size(LabelSize::XSmall)
                            .color(Color::Accent),
                    )
                })
                .child(
                    Label::new(tagline)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::new(IconName::Circle)
                                .size(IconSize::XSmall)
                                .color(dot_color),
                        )
                        .child(
                            Label::new(dot_label)
                                .size(LabelSize::XSmall)
                                .color(dot_color),
                        ),
                )
                .when_some(detail, |this, detail| {
                    this.child(
                        Label::new(Arc::<str>::from(detail))
                            .size(LabelSize::XSmall)
                            .color(detail_color),
                    )
                }),
        )
        // Cascade priority: install actions WIN over the "Selected"
        // decorative chip, even when the unhealthy adapter is the
        // user's current runtime.toml selection. If Bootstrap is
        // selected but its $PREFIX is empty (Phase 6 fresh-install
        // state), the user needs the Install button â€” showing a
        // "Selected" chip there would leave them stuck without a way
        // to trigger the download.
        .child(div().when(compact, |this| this.w_full()).child(
            if id == RuntimeId::Chroot && !matches!(entry.health, HealthStatus::Healthy) {
                // Chroot adapter requires the zdroid-spawnd Magisk module
                // to be running. If the daemon socket isn't reachable,
                // letting the user pick chroot just writes a runtime.toml
                // that breaks every subsequent spawn. Surface the install
                // path inline instead: tap "Get module" to jump to the
                // GitHub releases page where the zip lives. After install
                // + reboot, re-open the picker and the gate flips to
                // Healthy â†’ normal Select.
                Button::new(("get-module", idx), "Get module")
                    .when(compact, |this| this.full_width())
                    .end_icon(Icon::new(IconName::ArrowUpRight).size(IconSize::Small))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.open_url(SPAWND_RELEASE_URL);
                    }))
                    .into_any_element()
            } else if id == RuntimeId::Bootstrap
                && matches!(entry.health, HealthStatus::NotInstalled { .. })
            {
                // Bootstrap adapter has its 240 MB userland in a separate
                // GitHub repo (`<release_repo>`); Phase 6 of the Termux-
                // divestment refactor stopped bundling it in the APK and
                // moved download to `BootstrapAdapter::install`. Tap
                // "Install" to kick off the async download + extract; the
                // button label switches to the live `install_status` for
                // the duration. After completion the card flips to
                // Healthy â†’ normal Select.
                if let Some(status) = install_status {
                    Button::new(("installing", idx), status.to_string())
                        .when(compact, |this| this.full_width())
                        .disabled(true)
                        .into_any_element()
                } else {
                    Button::new(("install", idx), "Install")
                        .when(compact, |this| this.full_width())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.install_bootstrap(cx);
                        }))
                        .into_any_element()
                }
            } else if id == RuntimeId::ExternalTermux {
                Button::new(("external-termux-unavailable", idx), "Not available yet")
                    .when(compact, |this| this.full_width())
                    .disabled(true)
                    .into_any_element()
            } else if is_current {
                // Healthy AND the active selection â€” decorative confirm.
                // The header already shows an "Active" Chip; this right-
                // hand Chip is design-language parity.
                Chip::new("Selected")
                    .icon(IconName::Check)
                    .label_color(Color::Accent)
                    .into_any_element()
            } else {
                Button::new(("select", idx), "Select")
                    .when(compact, |this| this.full_width())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(id, window, cx);
                    }))
                    .into_any_element()
            },
        ))
        .into_any_element()
}

/// Build the per-adapter health snapshot at modal-open time. Each
/// adapter is constructed with on-device defaults so the health probe
/// can run; the user's eventual `runtime.toml` overrides these when
/// the adapter is actually selected.
fn build_entries() -> Vec<AdapterEntry> {
    let chroot_health = adapters::chroot::ChrootAdapter::new(default_chroot_config())
        .map(|a| a.health_check())
        .unwrap_or_else(|err| HealthStatus::Failed {
            error: err.to_string(),
        });
    let bootstrap_health = adapters::bootstrap::BootstrapAdapter::new(default_bootstrap_config())
        .map(|a| a.health_check())
        .unwrap_or_else(|err| HealthStatus::Failed {
            error: err.to_string(),
        });
    let termux_health =
        adapters::external_termux::ExternalTermuxAdapter::new(default_termux_config())
            .map(|a| a.health_check())
            .unwrap_or_else(|err| HealthStatus::Failed {
                error: err.to_string(),
            });

    vec![
        AdapterEntry {
            id: RuntimeId::Chroot,
            tagline: "Fastest. Routes through the persistent zd-spawnd daemon. Requires Magisk root + the zdroid-spawnd module.",
            health: chroot_health,
        },
        AdapterEntry {
            id: RuntimeId::Bootstrap,
            tagline: "Self-contained Termux-flavored userland inside Zdroid's sandbox. Bare or proot-wrapped. No root, no external app.",
            health: bootstrap_health,
        },
        AdapterEntry {
            id: RuntimeId::ExternalTermux,
            tagline: "Planned integration with the installed Termux app. Interactive stdio bridging is not implemented yet.",
            health: termux_health,
        },
    ]
}

fn default_chroot_config() -> ChrootConfig {
    ChrootConfig {
        root: PathBuf::from("/data/local/nhsystem/kali-arm64"),
        home_bind: PathBuf::from("/zed"),
        spawnd_socket: PathBuf::from("/data/data/com.zdroid/files/run/zd-spawn"),
        su_path: PathBuf::from("/product/bin/su"),
    }
}

fn default_bootstrap_config() -> BootstrapConfig {
    BootstrapConfig {
        prefix: PathBuf::from("/data/data/com.zdroid/files/usr"),
        proot_rootfs: None,
        release_repo: "Dylanmurzello/zdroid-bootstrap".into(),
    }
}

fn default_termux_config() -> ExternalTermuxConfig {
    ExternalTermuxConfig {
        package: "com.termux".into(),
        prefix: PathBuf::from("/data/data/com.termux/files/usr"),
    }
}

/// Read the active adapter id from `runtime.toml`. Returns `None` if
/// the file is missing (first-launch state) or unparseable; the picker
/// just doesn't render a "Current" badge in those cases.
fn detect_current() -> Option<RuntimeId> {
    RuntimeFile::load(std::path::Path::new(RUNTIME_TOML_PATH))
        .ok()
        .flatten()
        .map(|file| file.runtime.kind)
}
