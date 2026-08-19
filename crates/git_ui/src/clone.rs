use askpass::{AskPassDelegate, AskPassSession};
use futures::FutureExt;
use gpui::{App, Context, WeakEntity, Window};
use notifications::status_toast::StatusToast;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ui::{Color, Icon, IconName, SharedString};
use util::ResultExt;
use workspace::{self, Workspace};

/// Outcome of a single `git clone` invocation.
enum CloneOutcome {
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitCloneProgress {
    message: String,
    percent: Option<u8>,
}

#[derive(Default)]
struct GitCloneOutput {
    stderr: String,
    progress: Option<GitCloneProgress>,
}

static NEXT_CLONE_TASK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn show_clone_error(workspace: &mut Workspace, message: String, cx: &mut Context<Workspace>) {
    let toast = StatusToast::new(format!("Git Clone failed: {message}"), cx, |this, _| {
        this.icon(Icon::new(IconName::XCircle).color(Color::Error))
            .dismiss_button(true)
            .auto_dismiss(false)
    });
    workspace.toggle_status_toast(toast, cx);
}

fn redacted_repo_url(url: &str) -> String {
    let without_suffix = url.split(['?', '#']).next().unwrap_or(url);
    let Some((scheme, remainder)) = without_suffix.split_once("://") else {
        return without_suffix.to_owned();
    };
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let (authority, path) = remainder.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

fn clone_error_tail(stderr: &str) -> String {
    let lines = stderr
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty() && parse_git_progress(line).is_none())
        .rev()
        .take(6)
        .collect::<Vec<_>>();
    lines
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn parse_git_progress(line: &str) -> Option<GitCloneProgress> {
    const PHASES: [&str; 5] = [
        "Enumerating objects:",
        "Counting objects:",
        "Compressing objects:",
        "Receiving objects:",
        "Resolving deltas:",
    ];
    const CHECKOUT_PHASES: [&str; 2] = ["Updating files:", "Checking out files:"];

    let line = line.trim();
    let line = line.strip_prefix("remote: ").unwrap_or(line);
    if !PHASES
        .iter()
        .chain(CHECKOUT_PHASES.iter())
        .any(|phase| line.starts_with(phase))
    {
        return None;
    }
    let percent = line
        .split_once('%')
        .and_then(|(before, _)| before.split_whitespace().next_back())
        .and_then(|value| value.parse::<u8>().ok());
    Some(GitCloneProgress {
        message: line.to_owned(),
        percent,
    })
}

/// Spawns `git clone --progress <url>` in `cwd`, polls for exit or
/// cancellation. On cancel, sends `kill()` (SIGKILL on Unix) and waits
/// so we don't leave a zombie. The caller is responsible for
/// `remove_dir_all` of the cloned destination on the `Cancelled`
/// branch — git creates `<cwd>/<repo_name>` as it works, partially
/// populated until the operation finishes.
fn run_git_clone(
    url: &str,
    cwd: &Path,
    cancel: &AtomicBool,
    askpass_script: &std::ffi::OsStr,
    #[cfg(target_os = "android")] askpass_socket: &std::ffi::OsStr,
    output: Arc<Mutex<GitCloneOutput>>,
) -> anyhow::Result<CloneOutcome> {
    let safe_url = redacted_repo_url(url);
    log::info!(
        "git clone: starting url={safe_url} cwd={} PATH={}",
        cwd.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("git");
    #[cfg(target_os = "android")]
    command.arg("-c").arg("core.symlinks=false");
    let command = command
        .arg("clone")
        .arg("--progress")
        .arg(url)
        .current_dir(cwd)
        .env("GIT_ASKPASS", askpass_script)
        .env("SSH_ASKPASS", askpass_script)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stderr(Stdio::piped());
    #[cfg(target_os = "android")]
    command.env("ZED_ASKPASS_SOCKET", askpass_socket);
    let mut child = command.spawn().map_err(|err| {
        anyhow::anyhow!(
            "spawn git clone for {safe_url} in {} failed: {err} (PATH={})",
            cwd.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    })?;
    let mut stderr_reader = if let Some(mut stderr) = child.stderr.take() {
        let output = output.clone();
        Some(std::thread::spawn(move || {
            let mut reader = BufReader::new(&mut stderr);
            let mut chunk = Vec::new();
            while reader.read_until(b'\r', &mut chunk).unwrap_or(0) > 0 {
                let text = String::from_utf8_lossy(&chunk);
                if let Ok(mut output) = output.lock() {
                    output.stderr.push_str(&text);
                    for line in text.split(['\r', '\n']) {
                        if let Some(progress) = parse_git_progress(line) {
                            output.progress = Some(progress);
                        }
                    }
                }
                chunk.clear();
            }
        }))
    } else {
        None
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(stderr_reader) = stderr_reader.take() {
                let _ = stderr_reader.join();
            }
            log::info!("git clone: cancelled url={safe_url} cwd={}", cwd.display());
            return Ok(CloneOutcome::Cancelled);
        }
        match child.try_wait()? {
            Some(status) if status.success() => {
                if let Some(stderr_reader) = stderr_reader.take() {
                    let _ = stderr_reader.join();
                }
                log::info!("git clone: completed url={safe_url} cwd={}", cwd.display());
                return Ok(CloneOutcome::Completed);
            }
            Some(status) => {
                if let Some(stderr_reader) = stderr_reader.take() {
                    let _ = stderr_reader.join();
                }
                let stderr_tail = output
                    .lock()
                    .ok()
                    .map(|output| clone_error_tail(&output.stderr))
                    .unwrap_or_default();
                log::error!(
                    "git clone: failed url={safe_url} cwd={} status={status} stderr={stderr_tail}",
                    cwd.display(),
                );
                if stderr_tail.is_empty() {
                    anyhow::bail!("git clone exited with {status}")
                } else {
                    anyhow::bail!("git clone exited with {status}: {stderr_tail}")
                }
            }
            None => {
                // Block one background-pool thread for ~200ms between
                // polls. background_spawn pool is sized for blocking
                // workloads; using std::thread::sleep here avoids
                // pulling in a runtime-specific async timer for what's
                // a small per-iteration cost.
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

pub fn clone_and_open(
    repo_url: SharedString,
    workspace: WeakEntity<Workspace>,
    askpass: AskPassDelegate,
    window: &mut Window,
    cx: &mut App,
    on_success: Arc<
        dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send + Sync + 'static,
    >,
) {
    clone_and_open_with_destination(repo_url, None, workspace, askpass, window, cx, on_success);
}

pub fn clone_and_open_at(
    repo_url: SharedString,
    destination_dir: PathBuf,
    workspace: WeakEntity<Workspace>,
    askpass: AskPassDelegate,
    window: &mut Window,
    cx: &mut App,
    on_success: Arc<
        dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send + Sync + 'static,
    >,
) {
    clone_and_open_with_destination(
        repo_url,
        Some(destination_dir),
        workspace,
        askpass,
        window,
        cx,
        on_success,
    );
}

fn clone_and_open_with_destination(
    repo_url: SharedString,
    destination_dir: Option<PathBuf>,
    workspace: WeakEntity<Workspace>,
    askpass: AskPassDelegate,
    window: &mut Window,
    cx: &mut App,
    on_success: Arc<
        dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + Send + Sync + 'static,
    >,
) {
    let destination_prompt = destination_dir.is_none().then(|| {
        cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select as Repository Destination".into()),
        })
    });

    window
        .spawn(cx, async move |cx| {
            let mut destination_dir = match destination_dir {
                Some(destination_dir) => destination_dir,
                None => {
                    let mut paths = destination_prompt?.await.ok()?.ok()??;
                    paths.pop()?
                }
            };
            let safe_repo_url = redacted_repo_url(&repo_url);
            log::info!(
                "git clone: destination selected url={} destination={}",
                safe_repo_url,
                destination_dir.display()
            );

            let askpass_session =
                match AskPassSession::new(cx.background_executor().clone(), askpass).await {
                    Ok(session) => session,
                    Err(error) => {
                        log::error!("git clone: failed to start askpass session: {error:#}");
                        workspace
                            .update(cx, |workspace, cx| {
                                show_clone_error(workspace, error.to_string(), cx);
                            })
                            .log_err();
                        return None;
                    }
                };
            let askpass_script = askpass_session.script_path().as_ref().to_owned();
            #[cfg(target_os = "android")]
            let askpass_socket = askpass_session.socket_path().as_ref().to_owned();

            let repo_name = repo_url
                .split('/')
                .next_back()
                .map(|name| name.strip_suffix(".git").unwrap_or(name))
                .unwrap_or("repository")
                .to_owned();
            let task_id = format!(
                "git-clone-{}",
                NEXT_CLONE_TASK_ID.fetch_add(1, Ordering::Relaxed)
            );
            let task_description = format!("Cloning {repo_name}");
            cx.update(|_, cx| cx.start_background_task(&task_id, &task_description))
                .ok()?;
            let loading_modal = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace::BackgroundOperationModal::show(
                        workspace,
                        "Cloning repository",
                        format!(
                            "Downloading {repo_name}. You can leave Zdroid-B and return later."
                        ),
                        window,
                        cx,
                    )
                })
                .ok()
                .flatten();

            // The blocking dialog is the single source of clone progress and
            // cancellation. Git writes progress to stderr using carriage
            // returns, so the worker records the latest phase for the UI loop.
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(modal) = &loading_modal {
                modal.update(cx, |modal, cx| modal.set_cancel_flag(cancel.clone(), cx));
            }

            let clone_outcome = {
                let url_for_worker = repo_url.to_string();
                let cwd_for_worker = destination_dir.clone();
                let cancel_for_worker = cancel.clone();
                let output = Arc::new(Mutex::new(GitCloneOutput::default()));
                let output_for_worker = output.clone();
                let clone_task = cx
                    .background_executor()
                    .spawn(async move {
                        run_git_clone(
                            &url_for_worker,
                            &cwd_for_worker,
                            &cancel_for_worker,
                            &askpass_script,
                            #[cfg(target_os = "android")]
                            &askpass_socket,
                            output_for_worker,
                        )
                    })
                    .fuse();
                futures::pin_mut!(clone_task);
                let mut last_progress = None;
                loop {
                    let timer = cx
                        .background_executor()
                        .timer(Duration::from_millis(200))
                        .fuse();
                    futures::pin_mut!(timer);
                    futures::select_biased! {
                        outcome = clone_task => break outcome,
                        _ = timer => {
                            let progress = output
                                .lock()
                                .ok()
                                .and_then(|output| output.progress.clone());
                            if progress != last_progress {
                                if let Some(progress) = &progress {
                                    if let Some(modal) = &loading_modal {
                                        modal.update(cx, |modal, cx| {
                                            modal.set_progress(
                                                progress.message.clone(),
                                                progress.percent,
                                                cx,
                                            );
                                        });
                                    }
                                    cx.update(|_, cx| {
                                        cx.start_background_task(
                                            &task_id,
                                            &format!("{task_description}: {}", progress.message),
                                        );
                                    })
                                    .ok();
                                }
                                last_progress = progress;
                            }
                        }
                    }
                }
            };

            // Keep the socket-backed AskPass proxy alive until git exits.
            drop(askpass_session);

            match clone_outcome {
                Ok(CloneOutcome::Completed) => {
                    // Fall through to the existing post-clone prompt.
                }
                Ok(CloneOutcome::Cancelled) => {
                    let cloned_dir = destination_dir.join(&repo_name);
                    if let Err(err) = std::fs::remove_dir_all(&cloned_dir) {
                        log::warn!(
                            "git clone cancel: cleanup of {} failed: {err:#}",
                            cloned_dir.display()
                        );
                    }
                    if let Some(modal) = &loading_modal {
                        modal.update(cx, |modal, cx| modal.complete(cx));
                    }
                    cx.update(|_, cx| {
                        cx.finish_background_task(&task_id, &task_description, false)
                    })
                    .ok();
                    return None;
                }
                Err(error) => {
                    log::error!(
                        "git clone: failed url={} destination={}: {error:#}",
                        safe_repo_url,
                        destination_dir.display()
                    );
                    let cloned_dir = destination_dir.join(&repo_name);
                    if let Err(err) = std::fs::remove_dir_all(&cloned_dir) {
                        log::warn!(
                            "git clone failure cleanup of {} skipped: {err:#}",
                            cloned_dir.display()
                        );
                    }
                    workspace
                        .update(cx, |workspace, cx| {
                            show_clone_error(workspace, error.to_string(), cx);
                        })
                        .log_err();
                    if let Some(modal) = &loading_modal {
                        modal.update(cx, |modal, cx| modal.complete(cx));
                    }
                    cx.update(|_, cx| {
                        cx.finish_background_task(&task_id, &task_description, false)
                    })
                    .ok();
                    return None;
                }
            }

            let has_worktrees = workspace
                .read_with(cx, |workspace, cx| {
                    workspace.project().read(cx).worktrees(cx).next().is_some()
                })
                .ok()?;

            if has_worktrees {
                if let Some(modal) = &loading_modal {
                    modal.update(cx, |modal, cx| modal.complete(cx));
                }
                cx.update(|_, cx| cx.finish_background_task(&task_id, &task_description, true))
                    .ok();
            } else if let Some(modal) = &loading_modal {
                modal.update(cx, |modal, cx| {
                    modal.set_message(
                        "Updating project files. Large repositories may take a moment.",
                        cx,
                    )
                });
            }

            let prompt_answer = if has_worktrees {
                cx.update(|window, cx| {
                    window.prompt(
                        gpui::PromptLevel::Info,
                        &format!("Git Clone: {}", repo_name),
                        None,
                        &["Add repo to project", "Open repo in new project"],
                        cx,
                    )
                })
                .ok()?
                .await
                .ok()?
            } else {
                // Don't ask if project is empty
                0
            };

            destination_dir.push(&repo_name);

            match prompt_answer {
                0 => {
                    let create_task = workspace
                        .update_in(cx, |workspace, _window, cx| {
                            workspace.project().update(cx, |project, cx| {
                                project.create_worktree(destination_dir.as_path(), true, cx)
                            })
                        })
                        .ok()?;
                    let created = create_task.await.log_err().is_some();
                    if created {
                        workspace
                            .update_in(cx, |workspace, window, cx| {
                                (on_success)(workspace, window, cx);
                            })
                            .ok();
                    }
                    if !has_worktrees {
                        if let Some(modal) = &loading_modal {
                            modal.update(cx, |modal, cx| modal.complete(cx));
                        }
                        cx.update(|_, cx| {
                            cx.finish_background_task(&task_id, &task_description, created)
                        })
                        .ok();
                    }
                }
                1 => {
                    workspace
                        .update(cx, move |workspace, cx| {
                            let app_state = workspace.app_state().clone();
                            let destination_path = destination_dir.clone();
                            let on_success = on_success.clone();

                            workspace::open_new(
                                Default::default(),
                                app_state,
                                cx,
                                move |workspace, window, cx| {
                                    cx.activate(true);

                                    let create_task =
                                        workspace.project().update(cx, |project, cx| {
                                            project.create_worktree(
                                                destination_path.as_path(),
                                                true,
                                                cx,
                                            )
                                        });

                                    let workspace_weak = cx.weak_entity();
                                    cx.spawn_in(window, async move |_window, cx| {
                                        if create_task.await.log_err().is_some() {
                                            workspace_weak
                                                .update_in(cx, |workspace, window, cx| {
                                                    (on_success)(workspace, window, cx);
                                                })
                                                .ok();
                                        }
                                    })
                                    .detach();
                                },
                            )
                            .detach();
                        })
                        .ok();
                }
                _ => {}
            }

            Some(())
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::{clone_error_tail, parse_git_progress, redacted_repo_url};

    #[test]
    fn redacts_credentials_and_url_suffixes_from_clone_logs() {
        assert_eq!(
            redacted_repo_url("https://user:secret@github.com/acme/repo.git?token=secret#main"),
            "https://github.com/acme/repo.git"
        );
        assert_eq!(
            redacted_repo_url("git@github.com:acme/repo.git"),
            "git@github.com:acme/repo.git"
        );
    }

    #[test]
    fn parses_checkout_progress() {
        let progress = parse_git_progress("Updating files:  61% (2650/4344)").unwrap();
        assert_eq!(progress.percent, Some(61));
        assert_eq!(progress.message, "Updating files:  61% (2650/4344)");
    }

    #[test]
    fn clone_error_omits_progress_lines() {
        let stderr = concat!(
            "Receiving objects: 50% (5/10)\r",
            "Updating files: 100% (10/10), done.\n",
            "error: unable to create symlink AGENTS.md: File name too long\n",
        );
        assert_eq!(
            clone_error_tail(stderr),
            "error: unable to create symlink AGENTS.md: File name too long"
        );
    }
}
