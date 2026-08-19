//! Install the `zed-askpass-helper` binary from APK assets to the
//! runtime-shared `$PREFIX/.zed/bin` path.
//!
//! The askpass crate (`crates/askpass/src/askpass.rs`) sets `SSH_ASKPASS`
//! to a generated script that execs whatever path `askpass::set_askpass_program`
//! registered. Desktop platforms use `current_exe()` (the zed binary
//! itself, with `--askpass=<sock>`). On Android `current_exe()` is
//! `/system/bin/app_process64` (the Zygote launcher hosting the DEX
//! runtime), and ssh exec'ing it outside of an Activity context aborts
//! under SELinux `untrusted_app_27` with `Error changing dalvik-cache
//! ownership: Permission denied`. The helper here is a tiny standalone
//! aarch64 ELF (no JVM, no DEX) that does the same netcat-style relay
//! the desktop `askpass::main` does: read prompt from stdin, write to a
//! Unix socket the gpui process is listening on, read the password
//! back, print it to stdout.
//!
//! Before Phase 2 of the Termux-divestment refactor, this binary
//! installed to `$PREFIX/bin/zed-askpass-helper` via
//! `termux_bootstrap::install_askpass_helper`. That path was tied to
//! the bootstrap-flavored layout — chroot-only users still got a
//! redundant copy in their bootstrap $PREFIX, and Phase 4 (relocate
//! zd-exec/zd-runtime off $PREFIX) would have needed a follow-up sweep.
//! Managed Linux only exposes the Bootstrap prefix and shared HOME. Keeping
//! the helper below `$PREFIX/.zed/bin` therefore lets both Android/Bionic and
//! the Ubuntu guest execute the same binary during Git authentication.
//!
//! Idempotent: a byte-length comparison decides whether to re-extract.

use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use android_activity::AndroidApp;
use anyhow::{Context, Result, anyhow};

/// Asset name inside the APK. Matches the staging path the Cargo build
/// emits in `crates/gpui_android/examples/zed_android/build.rs`.
const ASSET_NAME: &str = "zed-askpass-helper";

/// Return the `/data/data` alias for app-private storage. Android commonly
/// reports `/data/user/0`, but Managed Linux exposes the former path.
pub fn runtime_shared_data_path(data_path: &Path) -> PathBuf {
    data_path
        .strip_prefix("/data/user/0")
        .map(|relative| Path::new("/data/data").join(relative))
        .unwrap_or_else(|_| data_path.to_path_buf())
}

/// Ensure the askpass helper is present at
/// `<data_path>/usr/.zed/bin/<ASSET_NAME>`
/// and matches the APK-bundled asset. Returns the absolute path to the
/// installed binary so the caller can hand it to `askpass::set_askpass_program`.
pub fn ensure_installed(android_app: &AndroidApp, data_path: &Path) -> Result<PathBuf> {
    // One-time sweep: pre-Phase 2 of the Termux-divestment refactor
    // installed this same binary at `$PREFIX/bin/zed-askpass-helper`
    // (= `<data_path>/usr/bin/zed-askpass-helper`). Users upgrading
    // over time still have that orphan sitting on disk consuming ~500
    // KB. The new install location is app-private (no `usr/` prefix)
    // so anything previously seeded at the old path is dead bytes.
    // Best-effort remove; logged, not propagated, since this is
    // hygiene, not a precondition.
    let shared_data_path = runtime_shared_data_path(data_path);
    for stale in [
        shared_data_path.join("usr/bin/zed-askpass-helper"),
        shared_data_path.join(ASSET_NAME),
    ] {
        if stale.exists() {
            match fs::remove_file(&stale) {
                Ok(()) => log::info!("askpass_install: swept stale binary at {}", stale.display()),
                Err(err) => log::warn!(
                    "askpass_install: could not remove stale {}: {err}",
                    stale.display()
                ),
            }
        }
    }

    let target = shared_data_path.join("usr/.zed/bin").join(ASSET_NAME);

    let asset_manager = android_app.asset_manager();
    let asset_name = CString::new(ASSET_NAME)?;
    let mut asset = asset_manager.open(&asset_name).ok_or_else(|| {
        anyhow!("{ASSET_NAME} asset not present in APK; check the cargo build step bundled it")
    })?;
    let expected_len = asset.length();

    if let Ok(meta) = fs::metadata(&target)
        && meta.len() as usize == expected_len
    {
        log::debug!(
            "askpass_install: {} up to date ({} bytes); skipping",
            target.display(),
            expected_len,
        );
        return Ok(target);
    }

    let mut buf = Vec::with_capacity(expected_len);
    asset.read_to_end(&mut buf)?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let staging = target.with_extension("new");
    fs::write(&staging, &buf).with_context(|| format!("write staging {}", staging.display()))?;
    let mut perms = fs::metadata(&staging)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&staging, perms)
        .with_context(|| format!("chmod 0755 {}", staging.display()))?;
    fs::rename(&staging, &target)
        .with_context(|| format!("rename {} -> {}", staging.display(), target.display()))?;

    log::info!(
        "askpass_install: installed askpass helper ({} bytes) at {}",
        buf.len(),
        target.display()
    );
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_android_ce_storage_for_managed_linux() {
        assert_eq!(
            runtime_shared_data_path(Path::new("/data/user/0/com.zdroid/files")),
            PathBuf::from("/data/data/com.zdroid/files")
        );
        assert_eq!(
            runtime_shared_data_path(Path::new("/data/data/com.zdroid/files")),
            PathBuf::from("/data/data/com.zdroid/files")
        );
    }
}
