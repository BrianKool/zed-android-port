//! Storage Access Framework bridge.
//!
//! GPUI's `prompt_for_paths` / `prompt_for_new_path` traditionally pop a
//! native file dialog. On Android the analogue is launching an `Intent`
//! (`ACTION_OPEN_DOCUMENT_TREE`, `ACTION_OPEN_DOCUMENT`,
//! `ACTION_CREATE_DOCUMENT`) and getting the result back via
//! `ActivityResultLauncher`. MainActivity owns the launcher and calls
//! back into Rust through `Java_..._onPickerResult` once the user picks
//! something.
//!
//! We translate the resulting `content://com.android.externalstorage
//! .documents/tree/primary%3A...` URIs into POSIX paths under
//! `/storage/emulated/0` so the rest of Zed (RealFs, Project worktrees,
//! etc.) can use them unchanged. Non-primary volumes (e.g. SD cards)
//! aren't covered by this shortcut and the caller will see an error.

use std::path::PathBuf;
use std::sync::Mutex;

use android_activity::AndroidApp;
use anyhow::{Context as _, Result};
use futures::channel::oneshot;
use jni::{
    JavaVM,
    objects::{JObject, JString, JValue},
    sys::jboolean,
};

type PendingPathsSender = oneshot::Sender<Result<Option<Vec<PathBuf>>>>;
type PendingPathSender = oneshot::Sender<Result<Option<PathBuf>>>;

enum Pending {
    Paths(PendingPathsSender),
    NewPath(PendingPathSender),
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Launch `ACTION_OPEN_DOCUMENT_TREE` and resolve the sender with the
/// picked tree path, or `Ok(None)` if the user cancelled.
pub(crate) fn pick_folder(
    android_app: &AndroidApp,
    sender: PendingPathsSender,
    import_foreign_trees: bool,
    force_import_tree: bool,
) {
    log::info!(
        "saf: pick_folder requested import_foreign_trees={} force_import_tree={}",
        import_foreign_trees,
        force_import_tree
    );
    set_pending(Pending::Paths(sender));
    if let Err(err) = launch_open_tree(android_app, import_foreign_trees, force_import_tree) {
        log::warn!("saf: launchOpenTree failed: {err:#}");
        if let Some(Pending::Paths(sender)) = PENDING.lock().unwrap().take() {
            let _ = sender.send(Err(err));
        }
    }
}

/// Launch `ACTION_OPEN_DOCUMENT` and resolve the sender with the picked
/// file path, or `Ok(None)` if the user cancelled.
pub(crate) fn pick_file(android_app: &AndroidApp, sender: PendingPathsSender) {
    log::info!("saf: pick_file requested");
    set_pending(Pending::Paths(sender));
    if let Err(err) = launch_open_document(android_app) {
        log::warn!("saf: launchOpenDocument failed: {err:#}");
        if let Some(Pending::Paths(sender)) = PENDING.lock().unwrap().take() {
            let _ = sender.send(Err(err));
        }
    }
}

/// Launch `ACTION_CREATE_DOCUMENT` so the user can pick where to save a
/// new file. The picked URI is converted to a POSIX path.
pub(crate) fn pick_new_path(
    android_app: &AndroidApp,
    suggested_name: Option<&str>,
    sender: PendingPathSender,
) {
    {
        let mut slot = PENDING.lock().unwrap();
        send_cancel(slot.take());
        *slot = Some(Pending::NewPath(sender));
    }
    if let Err(err) = launch_create_document(android_app, suggested_name) {
        log::warn!("pick_new_path: launch failed: {err:#}");
        if let Some(Pending::NewPath(sender)) = PENDING.lock().unwrap().take() {
            let _ = sender.send(Err(err));
        }
    }
}

fn set_pending(pending: Pending) {
    {
        let mut slot = PENDING.lock().unwrap();
        send_cancel(slot.take());
        *slot = Some(pending);
    }
}

fn send_cancel(p: Option<Pending>) {
    match p {
        Some(Pending::Paths(s)) => {
            let _ = s.send(Ok(None));
        }
        Some(Pending::NewPath(s)) => {
            let _ = s.send(Ok(None));
        }
        None => {}
    }
}

fn launch_open_tree(
    android_app: &AndroidApp,
    import_foreign_trees: bool,
    force_import_tree: bool,
) -> Result<()> {
    log::info!(
        "saf: calling MainActivity.launchOpenTree(import_foreign_trees={} force_import_tree={})",
        import_foreign_trees,
        force_import_tree
    );
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };
    let import_foreign_trees: jboolean = if import_foreign_trees { 1 } else { 0 };
    let force_import_tree: jboolean = if force_import_tree { 1 } else { 0 };
    let result = env.call_method(
        &activity,
        "launchOpenTree",
        "(ZZ)V",
        &[
            JValue::Bool(import_foreign_trees),
            JValue::Bool(force_import_tree),
        ],
    );
    clear_java_exception(&mut env, "MainActivity.launchOpenTree", result)?;
    log::info!("saf: MainActivity.launchOpenTree() returned");
    Ok(())
}

fn launch_open_document(android_app: &AndroidApp) -> Result<()> {
    log::info!("saf: calling MainActivity.launchOpenDocument()");
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };
    let result = env.call_method(&activity, "launchOpenDocument", "()V", &[]);
    clear_java_exception(&mut env, "MainActivity.launchOpenDocument", result)?;
    log::info!("saf: MainActivity.launchOpenDocument() returned");
    Ok(())
}

fn launch_create_document(android_app: &AndroidApp, suggested_name: Option<&str>) -> Result<()> {
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };
    let name = env.new_string(suggested_name.unwrap_or("untitled"))?;
    let result = env.call_method(
        &activity,
        "launchCreateDocument",
        "(Ljava/lang/String;)V",
        &[(&name).into()],
    );
    clear_java_exception(&mut env, "MainActivity.launchCreateDocument", result)?;
    Ok(())
}

fn clear_java_exception<T>(
    env: &mut jni::JNIEnv<'_>,
    operation: &str,
    result: jni::errors::Result<T>,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                env.exception_clear()
                    .with_context(|| format!("clear Java exception after {operation}"))?;
            }
            Err(anyhow::anyhow!("{operation} failed: {err}"))
        }
    }
}

/// Called from MainActivity's ActivityResultLauncher callback.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_MainActivity_onPickerResult<'local>(
    mut env: jni::JNIEnv<'local>,
    _activity: JObject<'local>,
    uri_string: JString<'local>,
) {
    let uri: String = match env.get_string(&uri_string) {
        Ok(s) => s.into(),
        Err(err) => {
            log::warn!("saf: couldn't read URI string from JVM: {err:#}");
            return;
        }
    };
    log::info!("saf: onPickerResult uri={uri:?}");
    let pending = PENDING.lock().unwrap().take();
    match pending {
        Some(Pending::Paths(sender)) => {
            let _ = sender.send(handle_tree_result(&uri).map(|p| p.map(|p| vec![p])));
        }
        Some(Pending::NewPath(sender)) => {
            let _ = sender.send(handle_document_result(&uri));
        }
        None => log::warn!("saf: onPickerResult fired with no pending sender"),
    }
}

fn handle_tree_result(uri: &str) -> Result<Option<PathBuf>> {
    if uri.is_empty() {
        return Ok(None);
    }
    if let Some(message) = uri.strip_prefix("zdroid-error:") {
        anyhow::bail!("{}", percent_decode(message));
    }
    if let Some(rest) = uri.strip_prefix("content://com.android.externalstorage.documents/tree/") {
        return Ok(Some(decode_storage_segment(rest)?));
    }
    if let Some(rest) = uri.strip_prefix("content://com.zdroid.documents/tree/") {
        return Ok(Some(decode_zed_segment(rest)?));
    }
    Err(anyhow::anyhow!("unsupported tree URI authority: {uri}"))
}

fn handle_document_result(uri: &str) -> Result<Option<PathBuf>> {
    if uri.is_empty() {
        return Ok(None);
    }
    if let Some(message) = uri.strip_prefix("zdroid-error:") {
        anyhow::bail!("{}", percent_decode(message));
    }
    if let Some(rest) =
        uri.strip_prefix("content://com.android.externalstorage.documents/document/")
    {
        return Ok(Some(decode_storage_segment(rest)?));
    }
    if let Some(rest) = uri.strip_prefix("content://com.zdroid.documents/document/") {
        return Ok(Some(decode_zed_segment(rest)?));
    }
    Err(anyhow::anyhow!("unsupported document URI authority: {uri}"))
}

/// Document IDs from `ZedDocumentsProvider` are already absolute
/// filesystem paths under our app-private dir
/// (`/data/data/<pkg>/files/home/...`). We're the only process that can
/// read them, but since the URI came from our own provider (or via the
/// system DocumentsUI which signed off on the user's choice), we just
/// percent-decode and use directly.
fn decode_zed_segment(segment: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(percent_decode(segment)))
}

fn decode_storage_segment(segment: &str) -> Result<PathBuf> {
    let decoded = percent_decode(segment);
    let (volume, rel) = decoded
        .split_once(':')
        .with_context(|| format!("malformed storage URI segment: {segment}"))?;
    let root = if volume == "primary" {
        PathBuf::from("/storage/emulated/0")
    } else {
        PathBuf::from(format!("/storage/{volume}"))
    };
    Ok(if rel.is_empty() { root } else { root.join(rel) })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
