use gpui::{App, Context, Window, actions};
use workspace::Workspace;

actions!(zdroid_danger_zone, [OpenDangerZone]);

pub fn register(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(open_danger_zone);
    })
    .detach();
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
