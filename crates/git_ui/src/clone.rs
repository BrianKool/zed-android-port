use askpass::{AskPassDelegate, AskPassSession};
use gpui::{App, Context, DismissEvent, WeakEntity, Window};
use notifications::status_toast::StatusToast;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use ui::{Color, Icon, IconName, IconSize, SharedString};
use util::ResultExt;
use workspace::{self, Workspace};

/// Outcome of a single `git clone` invocation.
enum CloneOutcome {
    Completed,
    Cancelled,
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
) -> anyhow::Result<CloneOutcome> {
    let safe_url = redacted_repo_url(url);
    log::info!(
        "git clone: starting url={safe_url} cwd={} PATH={}",
        cwd.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new("git")
        .arg("clone")
        .arg("--progress")
        .arg(url)
        .current_dir(cwd)
        .env("GIT_ASKPASS", askpass_script)
        .env("SSH_ASKPASS", askpass_script)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|err| {
            anyhow::anyhow!(
                "spawn git clone for {safe_url} in {} failed: {err} (PATH={})",
                cwd.display(),
                std::env::var("PATH").unwrap_or_default()
            )
        })?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            log::info!("git clone: cancelled url={safe_url} cwd={}", cwd.display());
            return Ok(CloneOutcome::Cancelled);
        }
        match child.try_wait()? {
            Some(status) if status.success() => {
                log::info!("git clone: completed url={safe_url} cwd={}", cwd.display());
                return Ok(CloneOutcome::Completed);
            }
            Some(status) => {
                log::error!(
                    "git clone: failed url={safe_url} cwd={} status={status}",
                    cwd.display()
                );
                anyhow::bail!("git clone exited with {status}")
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
                                let toast = StatusToast::new(error.to_string(), cx, |this, _| {
                                    this.icon(
                                        Icon::new(IconName::XCircle)
                                            .size(IconSize::Small)
                                            .color(Color::Error),
                                    )
                                    .dismiss_button(true)
                                });
                                workspace.toggle_status_toast(toast, cx);
                            })
                            .log_err();
                        return None;
                    }
                };
            let askpass_script = askpass_session.script_path().as_ref().to_owned();

            let repo_name = repo_url
                .split('/')
                .next_back()
                .map(|name| name.strip_suffix(".git").unwrap_or(name))
                .unwrap_or("repository")
                .to_owned();

            // Cancel flag wired from the progress toast's Cancel action
            // through to `run_git_clone`'s per-poll check. Replaces the
            // previous silent `fs.git_clone(...).await` so the user gets
            // visual feedback during long clones and can actually abort
            // (vs. clicking the welcome button repeatedly thinking it
            // didn't fire). On cancel we `child.kill()` + remove the
            // partially-cloned destination — `<destination_dir>/<repo_name>`
            // is freshly created by git itself during this call, so
            // `remove_dir_all` only ever touches files we just produced.
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_for_button = cancel.clone();

            let progress_toast = workspace
                .update(cx, |workspace, cx| {
                    let toast = StatusToast::new(
                        format!("Cloning {repo_name}…"),
                        cx,
                        move |this, _cx| {
                            let cancel_clone = cancel_for_button.clone();
                            this.icon(
                                Icon::new(IconName::CloudDownload)
                                    .size(IconSize::Small)
                                    .color(Color::Info),
                            )
                            .auto_dismiss(false)
                            .action("Cancel", move |_, _| {
                                cancel_clone.store(true, Ordering::Relaxed);
                            })
                        },
                    );
                    workspace.toggle_status_toast(toast.clone(), cx);
                    toast
                })
                .ok()?;

            let clone_outcome = {
                let url_for_worker = repo_url.to_string();
                let cwd_for_worker = destination_dir.clone();
                let cancel_for_worker = cancel.clone();
                cx.background_executor()
                    .spawn(async move {
                        run_git_clone(
                            &url_for_worker,
                            &cwd_for_worker,
                            &cancel_for_worker,
                            &askpass_script,
                        )
                    })
                    .await
            };

            // Keep the socket-backed AskPass proxy alive until git exits.
            drop(askpass_session);

            // Always dismiss the progress toast — completion, cancel, or error.
            let _ = progress_toast.update(cx, |_, cx| cx.emit(DismissEvent));

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
                            let toast = StatusToast::new(error.to_string(), cx, |this, _| {
                                this.icon(
                                    Icon::new(IconName::XCircle)
                                        .size(IconSize::Small)
                                        .color(Color::Error),
                                )
                                .dismiss_button(true)
                            });
                            workspace.toggle_status_toast(toast, cx);
                        })
                        .log_err();
                    return None;
                }
            }

            let has_worktrees = workspace
                .read_with(cx, |workspace, cx| {
                    workspace.project().read(cx).worktrees(cx).next().is_some()
                })
                .ok()?;

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
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            let create_task = workspace.project().update(cx, |project, cx| {
                                project.create_worktree(destination_dir.as_path(), true, cx)
                            });

                            let workspace_weak = cx.weak_entity();
                            let on_success = on_success.clone();
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
                        })
                        .ok()?;
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
    use super::redacted_repo_url;

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
}
