use gpui::{App, Context, Window, actions};
use project::Project;
use std::path::PathBuf;
use workspace::Workspace;

actions!(zdroid_danger_zone, [OpenDangerZone]);

pub fn register(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        workspace.register_action(open_danger_zone);
        let project = workspace.project().clone();
        write_active_project_roots(&project, cx);
        let observed_project = project.clone();
        cx.observe(&project, move |_workspace, _event, cx| {
            write_active_project_roots(&observed_project, cx);
        })
        .detach();
    })
    .detach();
}

fn write_active_project_roots(project: &gpui::Entity<Project>, cx: &App) {
    let roots = project
        .read(cx)
        .worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect::<Vec<_>>();
    let Some(prefix) = std::env::var_os("PREFIX").map(PathBuf::from) else {
        return;
    };
    let Some(files_dir) = prefix.parent() else {
        return;
    };
    let policy_dir = files_dir.join("policies");
    if let Err(error) = std::fs::create_dir_all(&policy_dir) {
        log::error!("could not create project policy directory: {error}");
        return;
    }
    let target = policy_dir.join("active-project-roots.txt");
    let temporary = policy_dir.join("active-project-roots.tmp");
    let body = roots
        .iter()
        .map(|root| root.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(error) =
        std::fs::write(&temporary, body).and_then(|_| std::fs::rename(&temporary, &target))
    {
        let _ = std::fs::remove_file(&temporary);
        log::error!("could not update active project policy: {error}");
    }
}

fn open_danger_zone(
    _workspace: &mut Workspace,
    _: &OpenDangerZone,
    _window: &mut Window,
    _cx: &mut Context<Workspace>,
) {
    if let Err(error) = show_android_dialog() {
        log::error!("could not open Zdroid-B Danger Zone: {error:#}");
    }
}

fn show_android_dialog() -> anyhow::Result<()> {
    use jni::JavaVM;

    let android_app = gpui_android::updater::registered_android_app()?;
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr() as *mut _)? };
    let mut env = vm.attach_current_thread()?;
    env.call_static_method(
        "com/zdroid/MainActivity",
        "showDangerZoneSettings",
        "()V",
        &[],
    )?;
    if env.exception_check()? {
        env.exception_describe()?;
        env.exception_clear()?;
        anyhow::bail!("Android rejected the Danger Zone dialog request");
    }
    Ok(())
}
