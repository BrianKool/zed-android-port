//! Keystore-backed Git credential bridge for Android terminal sessions.

use std::{
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt as _,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use android_activity::AndroidApp;
use anyhow::{Context as _, Result};

const CREDENTIAL_KEY: &str = "https://github.com";
const SOCKET_NAME: &str = "github-credential.sock";
const HELPER_NAME: &str = "zed-askpass-helper";
const GH_ENDPOINT_NAME: &str = "github-cli-credential";
const GH_WRAPPER_MARKER: &str = "# ZDROID_GH_CREDENTIAL_BRIDGE_V1";
const MAX_REQUEST_BYTES: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

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
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("secure credential socket {}", socket_path.display()))?;

    let gh_listener =
        TcpListener::bind(("127.0.0.1", 0)).context("bind GitHub CLI credential bridge")?;
    let gh_port = gh_listener
        .local_addr()
        .context("read GitHub CLI credential bridge address")?
        .port();
    let gh_nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    install_gh_bridge(data_path, gh_port, &gh_nonce)?;

    let app = android_app.clone();
    let thread_socket_path = socket_path.clone();
    std::thread::Builder::new()
        .name("github-credentials".into())
        .spawn(move || serve(listener, app, thread_socket_path))
        .context("spawn GitHub credential server")?;
    let app = android_app.clone();
    std::thread::Builder::new()
        .name("github-cli-credentials".into())
        .spawn(move || serve_gh(gh_listener, app, gh_nonce))
        .context("spawn GitHub CLI credential server")?;
    let _ = SERVER_STARTED.set(());
    Ok(())
}

fn install_gh_bridge(data_path: &Path, port: u16, nonce: &str) -> Result<()> {
    let home = data_path.join("home");
    let endpoint_dir = home.join(".local/share/zdroid");
    fs::create_dir_all(&endpoint_dir).context("create GitHub CLI bridge directory")?;
    fs::set_permissions(&endpoint_dir, fs::Permissions::from_mode(0o700))
        .context("secure GitHub CLI bridge directory")?;
    let endpoint = endpoint_dir.join(GH_ENDPOINT_NAME);
    write_private_file(
        &endpoint,
        format!("ZDROID_GH_PORT={port}\nZDROID_GH_NONCE={nonce}\n").as_bytes(),
        0o600,
    )?;

    ensure_gh_wrappers(data_path)
}

/// Recreate wrappers after Bootstrap or a Managed Linux rootfs has been
/// installed. Both installers replace directories that may not have existed
/// when the credential server first started.
pub fn ensure_gh_wrappers(data_path: &Path) -> Result<()> {
    let prefix_wrapper = data_path.join("usr/.zed/bin/gh");
    install_gh_wrapper(&prefix_wrapper)?;

    let runtime_dir = data_path.join("usr/var/lib/proot-distro");
    for root in [
        runtime_dir.join("containers"),
        runtime_dir.join("installed-rootfs"),
    ] {
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let current_rootfs = entry.path().join("rootfs");
                if current_rootfs.join("bin/sh").is_file() {
                    install_gh_wrapper(&current_rootfs.join("usr/local/bin/gh"))?;
                } else if entry.path().join("bin/sh").is_file() {
                    install_gh_wrapper(&entry.path().join("usr/local/bin/gh"))?;
                }
            }
        }
    }
    Ok(())
}

fn install_gh_wrapper(path: &Path) -> Result<()> {
    if path.exists() {
        let managed = fs::read_to_string(path)
            .map(|contents| contents.contains(GH_WRAPPER_MARKER))
            .unwrap_or(false);
        if !managed {
            log::warn!(
                "Not replacing user-managed GitHub CLI launcher at {}",
                path.display()
            );
            return Ok(());
        }
    }
    let parent = path.parent().context("GitHub CLI wrapper has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create GitHub CLI wrapper directory {}", parent.display()))?;
    write_private_file(path, gh_wrapper_contents(path).as_bytes(), 0o700)
        .with_context(|| format!("install GitHub CLI credential wrapper {}", path.display()))
}

fn gh_wrapper_contents(path: &Path) -> String {
    let bootstrap_gh = Path::new("usr").join(".zed").join("bin").join("gh");
    if path.ends_with(&bootstrap_gh) {
        format!(
            r#"#!/system/bin/sh
# ZDROID_GH_CREDENTIAL_BRIDGE_ANDROID_HOST

if [ -z "${{ZDROID_GH_WRAPPER_BASH:-}}" ]; then
    for zdroid_bash in "${{PREFIX:-}}/bin/bash" \
        "/data/data/com.zdroid/files/usr/bin/bash" \
        "/data/user/0/com.zdroid/files/usr/bin/bash"; do
        if [ -x "$zdroid_bash" ]; then
            export ZDROID_GH_WRAPPER_BASH=1
            exec "$zdroid_bash" "$0" "$@"
        fi
    done
    printf '%s\n' 'Zdroid-B: bash is required before GitHub CLI credentials can run.' >&2
    exit 127
fi
unset ZDROID_GH_WRAPPER_BASH

{GH_WRAPPER_BODY}"#
        )
    } else {
        format!("#!/usr/bin/env bash\n{GH_WRAPPER_BODY}")
    }
}

fn write_private_file(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let temporary = path.with_extension("zdroid-tmp");
    fs::write(&temporary, contents)
        .with_context(|| format!("write temporary file {}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
        .with_context(|| format!("secure temporary file {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("publish private file {}", path.display()))
}

const GH_WRAPPER_BODY: &str = r#"
# ZDROID_GH_CREDENTIAL_BRIDGE_V1

if [ -x /usr/bin/gh ]; then
    zdroid_real_gh=/usr/bin/gh
elif [ -n "${PREFIX:-}" ] && [ -x "$PREFIX/bin/gh" ]; then
    zdroid_real_gh="$PREFIX/bin/gh"
else
    printf '%s\n' 'Zdroid-B: GitHub CLI is not installed. Install it with: pkg install gh' >&2
    exit 127
fi

zdroid_endpoint="$HOME/.local/share/zdroid/github-cli-credential"
if [ -r "$zdroid_endpoint" ]; then
    unset ZDROID_GH_PORT ZDROID_GH_NONCE
    . "$zdroid_endpoint"
    if [[ "${ZDROID_GH_PORT:-}" =~ ^[0-9]+$ ]] && [ -n "${ZDROID_GH_NONCE:-}" ]; then
        if exec 3<>"/dev/tcp/127.0.0.1/$ZDROID_GH_PORT" 2>/dev/null; then
            printf 'token %s\n' "$ZDROID_GH_NONCE" >&3
            IFS= read -r zdroid_gh_token <&3 || true
            exec 3>&- 3<&-
            if [ -n "${zdroid_gh_token:-}" ]; then
                GH_TOKEN="$zdroid_gh_token" exec "$zdroid_real_gh" "$@"
            fi
        fi
    fi
fi

exec "$zdroid_real_gh" "$@"
"#;

fn serve(listener: UnixListener, android_app: AndroidApp, socket_path: PathBuf) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_request(stream, &android_app),
            Err(error) => log::warn!("GitHub credential socket accept failed: {error}"),
        }
    }
    let _ = fs::remove_file(socket_path);
}

fn serve_gh(listener: TcpListener, android_app: AndroidApp, nonce: String) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_gh_request(stream, &android_app, &nonce),
            Err(error) => log::warn!("GitHub CLI credential socket accept failed: {error}"),
        }
    }
}

fn serve_gh_request(mut stream: TcpStream, android_app: &AndroidApp, nonce: &str) {
    if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
    {
        return;
    }

    let mut request = String::new();
    let read_result = BufReader::new(&mut stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_line(&mut request);
    if read_result.is_err() || request.len() as u64 > MAX_REQUEST_BYTES {
        return;
    }
    let Some(candidate) = request.trim_end().strip_prefix("token ") else {
        return;
    };
    if candidate != nonce {
        return;
    }

    match crate::credentials::read(android_app, CREDENTIAL_KEY) {
        Ok(Some((_username, token))) => {
            if let Err(error) = stream
                .write_all(&token)
                .and_then(|_| stream.write_all(b"\n"))
            {
                log::warn!("GitHub CLI credential response write failed: {error}");
            }
        }
        Ok(None) => {
            let _ = stream.write_all(b"\n");
        }
        Err(error) => log::warn!("GitHub CLI credential Keystore read failed: {error:#}"),
    }
}

fn serve_request(mut stream: UnixStream, android_app: &AndroidApp) {
    if let Err(error) = stream.set_read_timeout(Some(IO_TIMEOUT)) {
        log::warn!("GitHub credential request timeout setup failed: {error}");
        return;
    }
    if let Err(error) = stream.set_write_timeout(Some(IO_TIMEOUT)) {
        log::warn!("GitHub credential response timeout setup failed: {error}");
        return;
    }

    let mut request = String::new();
    if let Err(error) = (&mut stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_string(&mut request)
    {
        log::warn!("GitHub credential request read failed: {error}");
        return;
    }
    if request.len() as u64 > MAX_REQUEST_BYTES {
        log::warn!("GitHub credential request exceeded {MAX_REQUEST_BYTES} bytes");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_gh_wrapper_uses_android_shell_entrypoint() {
        let wrapper = gh_wrapper_contents(Path::new("/data/data/com.zdroid/files/usr/.zed/bin/gh"));

        assert!(wrapper.starts_with("#!/system/bin/sh"));
        assert!(wrapper.contains("/data/data/com.zdroid/files/usr/bin/bash"));
        assert!(wrapper.contains("ZDROID_GH_CREDENTIAL_BRIDGE_V1"));
    }

    #[test]
    fn ubuntu_gh_wrapper_uses_linux_bash_entrypoint() {
        let wrapper = gh_wrapper_contents(Path::new(
            "/data/data/com.zdroid/files/usr/var/lib/proot-distro/containers/ubuntu/rootfs/usr/local/bin/gh",
        ));

        assert!(wrapper.starts_with("#!/usr/bin/env bash"));
        assert!(wrapper.contains("ZDROID_GH_CREDENTIAL_BRIDGE_V1"));
    }
}
