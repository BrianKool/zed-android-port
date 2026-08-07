//! Keystore-backed Git credential bridge for Android terminal sessions.

use std::{
    fs,
    io::{Read as _, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use android_activity::AndroidApp;
use anyhow::{Context as _, Result};

const CREDENTIAL_KEY: &str = "https://github.com";
const SOCKET_NAME: &str = "github-credential.sock";
const HELPER_NAME: &str = "zed-askpass-helper";

static SERVER_STARTED: OnceLock<()> = OnceLock::new();

pub fn terminal_env(data_path: &Path) -> Vec<(String, util::env::EnvOp)> {
    let helper = data_path.join(HELPER_NAME);
    let socket = data_path.join(SOCKET_NAME);
    vec![
        ("GIT_CONFIG_COUNT".into(), util::env::EnvOp::Set("1".into())),
        (
            "GIT_CONFIG_KEY_0".into(),
            util::env::EnvOp::Set("credential.https://github.com.helper".into()),
        ),
        (
            "GIT_CONFIG_VALUE_0".into(),
            util::env::EnvOp::Set(
                format!(
                    "!{} --git-credential={}",
                    helper.display(),
                    socket.display()
                )
                .into(),
            ),
        ),
    ]
}

pub fn start(android_app: &AndroidApp, data_path: &Path) -> Result<()> {
    if SERVER_STARTED.get().is_some() {
        return Ok(());
    }

    let socket_path = data_path.join(SOCKET_NAME);
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("remove stale credential socket {}", socket_path.display()))?;
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind credential socket {}", socket_path.display()))?;
    let app = android_app.clone();
    let thread_socket_path = socket_path.clone();
    std::thread::Builder::new()
        .name("github-credentials".into())
        .spawn(move || serve(listener, app, thread_socket_path))
        .context("spawn GitHub credential server")?;
    let _ = SERVER_STARTED.set(());
    Ok(())
}

fn serve(listener: UnixListener, android_app: AndroidApp, socket_path: PathBuf) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_request(stream, &android_app),
            Err(error) => log::warn!("GitHub credential socket accept failed: {error}"),
        }
    }
    let _ = fs::remove_file(socket_path);
}

fn serve_request(mut stream: UnixStream, android_app: &AndroidApp) {
    let mut request = String::new();
    if let Err(error) = stream.read_to_string(&mut request) {
        log::warn!("GitHub credential request read failed: {error}");
        return;
    }
    if !is_github_https_request(&request) {
        return;
    }

    match crate::credentials::read(android_app, CREDENTIAL_KEY) {
        Ok(Some((username, password))) => {
            let mut response = Vec::with_capacity(username.len() + password.len() + 22);
            response.extend_from_slice(b"username=");
            response.extend_from_slice(username.as_bytes());
            response.extend_from_slice(b"\npassword=");
            response.extend_from_slice(&password);
            response.extend_from_slice(b"\n\n");
            if let Err(error) = stream.write_all(&response) {
                log::warn!("GitHub credential response write failed: {error}");
            }
        }
        Ok(None) => {}
        Err(error) => log::warn!("GitHub credential Keystore read failed: {error:#}"),
    }
}

fn is_github_https_request(request: &str) -> bool {
    let mut protocol = None;
    let mut host = None;
    for line in request.lines() {
        if let Some(value) = line.strip_prefix("protocol=") {
            protocol = Some(value);
        } else if let Some(value) = line.strip_prefix("host=") {
            host = Some(value);
        }
    }
    protocol == Some("https") && matches!(host, Some("github.com" | "github.com:443"))
}
