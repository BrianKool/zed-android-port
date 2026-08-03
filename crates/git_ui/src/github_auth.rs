use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use editor::Editor;
use futures::AsyncReadExt as _;
use gpui::{
    App, BackgroundExecutor, ClipboardItem, Context, DismissEvent, EventEmitter, FocusHandle,
    Focusable, Render, SharedString, Window, actions,
};
use http_client::{AsyncBody, HttpClient, Method, Request};
use serde::Deserialize;
use ui::{
    Button, ButtonStyle, Color, Icon, IconName, IconSize, Label, LabelSize, Modal, ModalFooter,
    ModalHeader, Section, TintColor, prelude::*,
};
use workspace::{ModalView, Workspace};

actions!(github_auth, [OpenGithubAccounts]);

pub(crate) const GITHUB_CREDENTIALS_KEY: &str = "https://github.com";
const GITHUB_CLIENT_ID: &str = "Ov23lix5l83Pvw0bmit7";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";
const GITHUB_GIT_USERNAME: &str = "x-access-token";

#[derive(Clone, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn response_text(
    http_client: &Arc<dyn HttpClient>,
    request: Request<AsyncBody>,
) -> Result<(http_client::StatusCode, String)> {
    let mut response = http_client.send(request).await?;
    let status = response.status();
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    Ok((status, body))
}

async fn validate_token(http_client: &Arc<dyn HttpClient>, token: &str) -> Result<GithubUser> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(USER_URL)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .body(AsyncBody::default())?;
    let (status, body) = response_text(http_client, request).await?;
    if !status.is_success() {
        bail!("GitHub rejected this token ({status})");
    }
    serde_json::from_str(&body).context("GitHub returned an invalid user response")
}

async fn request_device_code(
    http_client: &Arc<dyn HttpClient>,
    executor: &BackgroundExecutor,
) -> Result<DeviceCodeResponse> {
    let mut last_error = None;
    for attempt in 1..=3 {
        let body = format!("client_id={GITHUB_CLIENT_ID}&scope=repo%20read%3Aorg");
        let request = Request::builder()
            .method(Method::POST)
            .uri(DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(AsyncBody::from(body.into_bytes()))?;
        match response_text(http_client, request).await {
            Ok((status, body)) => {
                if !status.is_success() {
                    bail!("GitHub device login failed ({status}): {body}");
                }
                return serde_json::from_str(&body)
                    .context("GitHub returned an invalid device login response");
            }
            Err(error) => {
                log::warn!("GitHub device login request attempt {attempt} failed: {error:#}");
                last_error = Some(error);
                if attempt < 3 {
                    executor.timer(Duration::from_secs(1)).await;
                }
            }
        }
    }

    Err(last_error.expect("device login must record a failed request"))
        .context("Could not connect to GitHub after 3 attempts")
}

async fn poll_for_access_token(
    http_client: &Arc<dyn HttpClient>,
    executor: &BackgroundExecutor,
    device: &DeviceCodeResponse,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = Duration::from_secs(device.interval.max(1));

    loop {
        if Instant::now() >= deadline {
            bail!("GitHub login expired. Start the login again.");
        }
        executor.timer(interval).await;
        let body = format!(
            "client_id={GITHUB_CLIENT_ID}&device_code={}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            device.device_code
        );
        let request = Request::builder()
            .method(Method::POST)
            .uri(ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(AsyncBody::from(body.into_bytes()))?;
        let (status, body) = match response_text(http_client, request).await {
            Ok(response) => response,
            Err(error) => {
                // Android may suspend Zdroid while the browser handles OAuth.
                // Retry stale connections after the app returns to the foreground.
                log::warn!("GitHub token polling request failed; retrying: {error:#}");
                continue;
            }
        };
        if !status.is_success() {
            bail!("GitHub token request failed ({status}): {body}");
        }

        let response: AccessTokenResponse =
            serde_json::from_str(&body).context("GitHub returned an invalid token response")?;
        if let Some(token) = response.access_token {
            return Ok(token);
        }
        match response.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("expired_token") => bail!("GitHub login expired. Start the login again."),
            Some("access_denied") => bail!("GitHub login was cancelled."),
            Some(error) => bail!(
                "GitHub login failed: {}",
                response.error_description.as_deref().unwrap_or(error)
            ),
            None => bail!("GitHub returned no token"),
        }
    }
}

enum AuthState {
    Checking,
    SignedOut,
    TokenEntry,
    Working(SharedString),
    DeviceCode {
        code: SharedString,
        verification_uri: SharedString,
    },
    SignedIn(SharedString),
    Error(SharedString),
}

pub(crate) struct GithubAccountsModal {
    state: AuthState,
    token_editor: gpui::Entity<Editor>,
    focus_handle: FocusHandle,
}

impl GithubAccountsModal {
    pub(crate) fn toggle(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, |window, cx| Self::new(window, cx));
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("GitHub personal access token", window, cx);
            editor.set_masked(true, cx);
            editor
        });
        let read_credentials = cx.read_credentials(GITHUB_CREDENTIALS_KEY);
        let http_client = cx.http_client();
        cx.spawn(async move |this, cx| {
            let state = match read_credentials.await {
                Ok(Some((username, token))) if username == GITHUB_GIT_USERNAME => {
                    match std::str::from_utf8(&token) {
                        Ok(token) => match validate_token(&http_client, token).await {
                            Ok(user) => AuthState::SignedIn(user.login.into()),
                            Err(error) => AuthState::Error(error.to_string().into()),
                        },
                        Err(_) => AuthState::Error("Stored GitHub credential is invalid.".into()),
                    }
                }
                Ok(Some((username, _))) => AuthState::SignedIn(username.into()),
                Ok(None) => AuthState::SignedOut,
                Err(error) => AuthState::Error(error.to_string().into()),
            };
            this.update(cx, |this, cx| {
                this.state = state;
                cx.notify();
            })
        })
        .detach();

        Self {
            state: AuthState::Checking,
            token_editor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn dismiss(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn show_token_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = AuthState::TokenEntry;
        self.token_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn login_with_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.token_editor.read(cx).text(cx).trim().to_string();
        if token.is_empty() {
            self.state = AuthState::Error("Enter a GitHub token first.".into());
            cx.notify();
            return;
        }
        self.token_editor
            .update(cx, |editor, cx| editor.clear(window, cx));
        self.state = AuthState::Working("Validating token...".into());
        cx.notify();

        let http_client = cx.http_client();
        cx.spawn(async move |this, cx| {
            let result: Result<GithubUser> = async {
                let user = validate_token(&http_client, &token).await?;
                let store = cx.update(|cx| {
                    cx.write_credentials(GITHUB_CREDENTIALS_KEY, &user.login, token.as_bytes())
                });
                store.await?;
                Ok(user)
            }
            .await;

            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(user) => AuthState::SignedIn(user.login.into()),
                    Err(error) => AuthState::Error(error.to_string().into()),
                };
                cx.notify();
            })
        })
        .detach();
    }

    fn login_via_github(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.state = AuthState::Working("Starting GitHub login...".into());
        cx.notify();
        let http_client = cx.http_client();
        let executor = cx.background_executor().clone();

        cx.spawn(async move |this, cx| {
            let result: Result<GithubUser> = async {
                let device = request_device_code(&http_client, &executor).await?;
                let code: SharedString = device.user_code.clone().into();
                let verification_uri: SharedString = device.verification_uri.clone().into();
                this.update(cx, |this, cx| {
                    this.state = AuthState::DeviceCode {
                        code: code.clone(),
                        verification_uri: verification_uri.clone(),
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                    cx.open_url(&verification_uri);
                    cx.notify();
                })?;

                let token = poll_for_access_token(&http_client, &executor, &device).await?;
                let user = validate_token(&http_client, &token).await?;
                let store = cx.update(|cx| {
                    cx.write_credentials(
                        GITHUB_CREDENTIALS_KEY,
                        GITHUB_GIT_USERNAME,
                        token.as_bytes(),
                    )
                });
                store.await?;
                Ok(user)
            }
            .await;

            if let Err(error) = &result {
                log::error!("GitHub device login failed: {error:#}");
            }

            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(user) => AuthState::SignedIn(user.login.into()),
                    Err(error) => AuthState::Error(format!("{error:#}").into()),
                };
                cx.notify();
            })
        })
        .detach();
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.state = AuthState::Working("Signing out...".into());
        cx.notify();
        let delete = cx.delete_credentials(GITHUB_CREDENTIALS_KEY);
        cx.spawn(async move |this, cx| {
            let result = delete.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(()) => AuthState::SignedOut,
                    Err(error) => AuthState::Error(error.to_string().into()),
                };
                cx.notify();
            })
        })
        .detach();
    }

    fn signed_out_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(
                Button::new("github-browser-login", "Log in via GitHub")
                    .full_width()
                    .style(ButtonStyle::Tinted(TintColor::Accent))
                    .start_icon(Icon::new(IconName::Github).size(IconSize::Small))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.login_via_github(window, cx);
                    })),
            )
            .child(
                Button::new("github-token-login", "Log in with Token")
                    .full_width()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_token_entry(window, cx);
                    })),
            )
    }
}

impl EventEmitter<DismissEvent> for GithubAccountsModal {}
impl ModalView for GithubAccountsModal {}

impl Focusable for GithubAccountsModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if matches!(self.state, AuthState::TokenEntry) {
            self.token_editor.focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }
}

impl Render for GithubAccountsModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.state {
            AuthState::Checking => v_flex()
                .gap_2()
                .child(Label::new("Checking GitHub account...").color(Color::Muted))
                .into_any_element(),
            AuthState::SignedOut => self.signed_out_actions(cx).into_any_element(),
            AuthState::TokenEntry => v_flex()
                .gap_3()
                .child(Label::new("Personal access token").size(LabelSize::Small))
                .child(
                    div()
                        .w_full()
                        .p_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .bg(cx.theme().colors().editor_background)
                        .child(self.token_editor.clone()),
                )
                .child(
                    Button::new("save-github-token", "Validate and Log In")
                        .full_width()
                        .style(ButtonStyle::Tinted(TintColor::Accent))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.login_with_token(window, cx);
                        })),
                )
                .child(
                    Button::new("cancel-github-token", "Back")
                        .full_width()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state = AuthState::SignedOut;
                            cx.notify();
                        })),
                )
                .into_any_element(),
            AuthState::Working(message) => v_flex()
                .items_center()
                .gap_3()
                .child(
                    Button::new("github-working", message.clone())
                        .loading(true)
                        .disabled(true),
                )
                .into_any_element(),
            AuthState::DeviceCode {
                code,
                verification_uri,
            } => v_flex()
                .gap_3()
                .child(Label::new("Finish authentication in your browser."))
                .child(
                    Button::new("copy-github-device-code", code.clone())
                        .full_width()
                        .style(ButtonStyle::Tinted(TintColor::Accent))
                        .start_icon(Icon::new(IconName::Copy).size(IconSize::Small))
                        .on_click({
                            let code = code.clone();
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                            }
                        }),
                )
                .child(
                    Label::new(verification_uri.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Button::new("github-waiting", "Waiting for GitHub...")
                        .loading(true)
                        .disabled(true),
                )
                .into_any_element(),
            AuthState::SignedIn(login) => v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Github)
                                .size(IconSize::Small)
                                .color(Color::Success),
                        )
                        .child(Label::new(format!("Signed in as {login}"))),
                )
                .child(
                    Button::new("github-logout", "Log Out")
                        .full_width()
                        .on_click(cx.listener(|this, _, _, cx| this.logout(cx))),
                )
                .into_any_element(),
            AuthState::Error(message) => v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .items_start()
                        .gap_2()
                        .child(
                            Icon::new(IconName::XCircle)
                                .size(IconSize::Small)
                                .color(Color::Error),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .w_full()
                                .whitespace_normal()
                                .child(Label::new(message.clone()).color(Color::Error)),
                        ),
                )
                .child(self.signed_out_actions(cx))
                .into_any_element(),
        };

        v_flex()
            .id("github-accounts-modal")
            .key_context("GithubAccountsModal")
            .w(rems(34.))
            .max_w_full()
            .elevation_3(cx)
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::dismiss))
            .child(
                Modal::new("github-accounts", None)
                    .header(
                        ModalHeader::new()
                            .headline("GitHub Accounts")
                            .description(
                                "Use GitHub authentication for HTTPS clone, fetch, pull, and push.",
                            )
                            .show_dismiss_button(true),
                    )
                    .section(Section::new().child(content))
                    .footer(ModalFooter::new().end_slot(
                        Button::new("close-github-accounts", "Close").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.dismiss(&menu::Cancel, window, cx);
                            },
                        )),
                    )),
            )
    }
}
