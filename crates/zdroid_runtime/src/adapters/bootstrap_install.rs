//! Download + extract path for [`super::bootstrap::BootstrapAdapter::install`].
//!
//! Pulls the latest release tarball from `<release_repo>`'s GitHub
//! releases endpoint, downloads `bootstrap-aarch64.zip`, extracts the
//! contents into a staging dir, swaps the staging into `$PREFIX`
//! atomically, and writes a version sentinel so subsequent boots skip
//! re-extraction.
//!
//! Lives in `zdroid_runtime` rather than `gpui_android` so the adapter
//! is self-contained — the historical extraction code in
//! `gpui_android::termux_bootstrap` couples to APK assets and the older
//! bundled-zip flow. Phase 8 of the Termux-divestment refactor sweeps
//! the legacy module; this is the replacement for the install path.

use std::fs;
use std::io::{Cursor, Read, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::health::ProgressSink;

/// Preferred asset name inside the GitHub release. The
/// bootstrap-build pipeline names every release's archive identically;
/// the version is encoded in the release TAG, not the asset filename.
///
/// On 404 we fall back to enumerating the release's assets via the
/// GitHub API and picking the first whose name matches
/// `ASSET_NAME_REGEX_PATTERN` (eg. `bootstrap-aarch64-r4.zip`). The
/// fallback path is robust to typos at upload time; the canonical
/// name keeps the steady-state path off `api.github.com`.
const RELEASE_ASSET_NAME: &str = "bootstrap-aarch64.zip";

/// Prefix + extension that any acceptable bootstrap asset must match.
/// The fallback asset-enumeration path picks the first asset whose
/// name starts with this prefix and ends with `.zip`. Covers
/// `bootstrap-aarch64.zip`, `bootstrap-aarch64-zdroid.zip`,
/// `bootstrap-aarch64-r4.zip`, etc.
const ASSET_NAME_PREFIX: &str = "bootstrap-aarch64";
const ASSET_NAME_SUFFIX: &str = ".zip";
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 300_000;

/// Manifest entry inside the bootstrap zip carrying symlink targets
/// the zip format itself can't represent on extraction (Android's
/// extract path doesn't preserve mode bits + symlinks the way `unzip`
/// does on a real Unix). Replayed after the regular extract pass.
const SYMLINKS_ENTRY: &str = "SYMLINKS.txt";
const SYMLINKS_DELIM: &str = "←";

/// File at `$PREFIX/.bootstrap-version` recording which release tag is
/// currently extracted. Re-extracts only fire when this doesn't match
/// the latest release tag at download time.
const VERSION_FILE: &str = ".bootstrap-version";
const DEPENDENCY_REPAIR_FILE: &str = ".dependencies-repaired-v2";

/// Download the latest release zip + extract into `<prefix>` atomically.
/// Idempotent against `<prefix>/.bootstrap-version` — if the on-disk
/// sentinel matches the latest tag, the function returns Ok without
/// touching the network or filesystem beyond a tag resolve.
pub fn install_latest(
    prefix: &Path,
    release_repo: &str,
    progress: &mut dyn ProgressSink,
) -> Result<()> {
    progress.step("Resolving latest bootstrap release");
    let tag_name = resolve_latest_tag(release_repo)
        .with_context(|| format!("resolve latest release tag for {release_repo}"))?;
    log::info!("bootstrap_install: latest release tag = {tag_name}");

    let version_file = prefix.join(VERSION_FILE);
    if let Ok(existing) = fs::read_to_string(&version_file)
        && existing.trim() == tag_name
    {
        ensure_package_manager_launchers(prefix)?;
        repair_bootstrap_dependencies(prefix, progress)?;
        log::info!("bootstrap_install: $PREFIX already at {tag_name}, skipping extract");
        progress.step(&format!("Bootstrap {tag_name} already installed"));
        return Ok(());
    }

    progress.step(&format!("Downloading bootstrap {tag_name}"));
    let zip_bytes = download_bootstrap_asset(release_repo, &tag_name, progress)
        .with_context(|| format!("download bootstrap asset for {release_repo} tag {tag_name}"))?;
    log::info!(
        "bootstrap_install: downloaded {} bytes for tag {}",
        zip_bytes.len(),
        tag_name,
    );

    progress.step("Extracting bootstrap");
    let staging = prefix.with_extension("staging");
    extract_into_staging(&zip_bytes, &staging)
        .with_context(|| format!("extract into {}", staging.display()))?;

    swap_staging_into_prefix(&staging, prefix)
        .with_context(|| format!("swap staging into {}", prefix.display()))?;

    ensure_package_manager_launchers(prefix)?;
    repair_bootstrap_dependencies(prefix, progress)?;

    if let Some(parent) = version_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(&version_file)
        .with_context(|| format!("create version sentinel at {}", version_file.display()))?
        .write_all(tag_name.as_bytes())?;

    log::info!(
        "bootstrap_install: bootstrap {} ready at {}",
        tag_name,
        prefix.display()
    );
    progress.step(&format!("Bootstrap {tag_name} installed"));
    Ok(())
}

/// Put package-manager shims ahead of `$PREFIX/bin` on Zdroid's PATH.
/// A newly unpacked Termux executable can run during dpkg cleanup before the
/// post-invoke RUNPATH hook patches it, so scope `$PREFIX/lib` to this process
/// tree instead of setting `LD_LIBRARY_PATH` globally for the Android app.
pub fn ensure_package_manager_launchers(prefix: &Path) -> Result<()> {
    let launcher_dir = prefix.join(".zed/bin");
    fs::create_dir_all(&launcher_dir)
        .with_context(|| format!("create launcher directory at {}", launcher_dir.display()))?;

    for tool in ["pkg", "apt", "apt-get"] {
        let target = prefix.join("bin").join(tool);
        if !target.is_file() {
            continue;
        }
        let launcher = launcher_dir.join(tool);
        let script = format!(
            "#!/system/bin/sh\n\
             export LD_LIBRARY_PATH=\"$PREFIX/lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n\
             export DEBIAN_FRONTEND=\"${{DEBIAN_FRONTEND:-noninteractive}}\"\n\
             exec \"$PREFIX/bin/{tool}\" \"$@\"\n"
        );
        fs::write(&launcher, script)
            .with_context(|| format!("write launcher at {}", launcher.display()))?;
        let mut permissions = fs::metadata(&launcher)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&launcher, permissions)
            .with_context(|| format!("chmod launcher at {}", launcher.display()))?;
    }

    let dpkg_launcher = launcher_dir.join("zdroid-dpkg");
    let dpkg_script = r#"#!/system/bin/sh
PREFIX=${PREFIX:-/data/data/com.zdroid/files/usr}
export PREFIX
export TERMUX_APP__PACKAGE_NAME=com.zdroid
export LD_LIBRARY_PATH="$PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

rewrite_dpkg_metadata() {
    info="$PREFIX/var/lib/dpkg/info"
    if [ -d "$info" ]; then
        find "$info" -type f -exec "$PREFIX/bin/sed" -i \
            's|/data/data/com\.termux/|/data/data/com.zdroid/|g' {} + 2>/dev/null || true
    fi
    status="$PREFIX/var/lib/dpkg/status"
    if [ -f "$status" ]; then
        "$PREFIX/bin/sed" -i \
            's|/data/data/com\.termux/|/data/data/com.zdroid/|g' "$status" 2>/dev/null || true
    fi
}

# Rewrite incoming package archives before dpkg sees them. This covers direct
# `dpkg -i` as well as apt's unpack calls, including transactions that unpack
# and configure in one process where a post-invoke hook would be too late.
preinstall="$PREFIX/etc/apt/zed-pre-install-rewrite.sh"
has_debs=0
for argument in "$@"; do
    case "$argument" in
        *.deb) has_debs=1 ;;
    esac
done
if [ "$has_debs" -eq 1 ]; then
    if [ ! -x "$preinstall" ]; then
        echo "Zdroid-B: missing incoming package rewrite hook: $preinstall" >&2
        exit 70
    fi
    for argument in "$@"; do
        case "$argument" in
            *.deb) printf '%s\n' "$argument" ;;
        esac
    done | "$preinstall"

    # Refuse to start a transaction if control metadata still names the
    # inaccessible Termux sandbox. Data binaries are handled by the ELF hook;
    # this check targets maintainer scripts and conffiles that dpkg may execute
    # or inspect immediately.
    for argument in "$@"; do
        case "$argument" in
            *.deb)
                verify_dir=$(mktemp -d) || exit 70
                if ! "$PREFIX/bin/dpkg-deb" -e "$argument" "$verify_dir" >/dev/null 2>&1; then
                    rm -rf "$verify_dir"
                    echo "Zdroid-B: could not inspect package metadata: $argument" >&2
                    exit 70
                fi
                if grep -rlI '/data/data/com\.termux/' "$verify_dir" >/dev/null 2>&1; then
                    rm -rf "$verify_dir"
                    echo "Zdroid-B: unsafe com.termux metadata remains in: $argument" >&2
                    exit 70
                fi
                rm -rf "$verify_dir"
                ;;
        esac
    done
fi

# Before fixes a transaction interrupted after unpack. After makes metadata
# from incoming Termux packages safe before apt starts its configure pass.
rewrite_dpkg_metadata
"$PREFIX/bin/dpkg" "$@"
result=$?
rewrite_dpkg_metadata
exit "$result"
"#;
    fs::write(&dpkg_launcher, dpkg_script)
        .with_context(|| format!("write launcher at {}", dpkg_launcher.display()))?;
    let mut permissions = fs::metadata(&dpkg_launcher)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&dpkg_launcher, permissions)
        .with_context(|| format!("chmod launcher at {}", dpkg_launcher.display()))?;

    let user_dpkg_launcher = launcher_dir.join("dpkg");
    fs::write(
        &user_dpkg_launcher,
        "#!/system/bin/sh\nexec \"$PREFIX/.zed/bin/zdroid-dpkg\" \"$@\"\n",
    )
    .with_context(|| format!("write launcher at {}", user_dpkg_launcher.display()))?;
    let mut permissions = fs::metadata(&user_dpkg_launcher)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&user_dpkg_launcher, permissions)
        .with_context(|| format!("chmod launcher at {}", user_dpkg_launcher.display()))?;

    let apt_config = prefix.join("etc/apt/apt.conf.d/96-zdroid-dpkg-wrapper");
    if let Some(parent) = apt_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &apt_config,
        format!(
            "Dir::Bin::dpkg \"{}\";\n\
             Dpkg::Options {{\n\
               \"--force-confdef\";\n\
               \"--force-confold\";\n\
             }};\n",
            dpkg_launcher.to_string_lossy()
        ),
    )
    .with_context(|| format!("write apt config at {}", apt_config.display()))?;
    Ok(())
}

fn bootstrap_env(prefix: &Path) -> Result<(PathBuf, PathBuf, String)> {
    let home = prefix
        .parent()
        .map(|files| files.join("home"))
        .unwrap_or_else(|| prefix.join("home"));
    let tmp = prefix.join("tmp");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&tmp)?;
    let path = format!(
        "{}:{}",
        prefix.join(".zed/bin").display(),
        prefix.join("bin").display()
    );
    Ok((home, tmp, path))
}

fn run_bootstrap_command(prefix: &Path, label: &str, program: &Path, args: &[&str]) -> Result<()> {
    let (home, tmp, path) = bootstrap_env(prefix)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env("PREFIX", prefix)
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("DEBIAN_FRONTEND", "noninteractive")
        // Keep this scoped to bootstrap package-manager children. Setting it
        // on Zdroid globally can make Android system binaries load Termux
        // libraries, while omitting it makes dpkg cleanup fail when a freshly
        // unpacked coreutils binary has not had its RUNPATH rewritten yet.
        .env("LD_LIBRARY_PATH", prefix.join("lib"))
        .env("PATH", path);

    let termux_exec = prefix.join("lib/libtermux-exec.so");
    if termux_exec.is_file() {
        command.env("LD_PRELOAD", termux_exec);
    }

    let output = command
        .output()
        .with_context(|| format!("run {label} using {}", program.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(anyhow!(
        "{label} failed ({}): {}{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    ))
}

/// Published bootstrap images currently contain a few preinstalled packages
/// whose declared dependencies are not included in the archive (`clang` for
/// golang/dpkg-perl and `rust-src` for rust-analyzer). Repair once immediately
/// after extraction so the user's first `pkg upgrade` starts from a consistent
/// dpkg state.
fn repair_bootstrap_dependencies(prefix: &Path, progress: &mut dyn ProgressSink) -> Result<()> {
    let marker = prefix.join(DEPENDENCY_REPAIR_FILE);
    if marker.is_file() {
        return Ok(());
    }

    let apt_get = prefix.join("bin/apt-get");
    if !apt_get.is_file() {
        return Err(anyhow!(
            "{} is missing after bootstrap extraction",
            apt_get.display()
        ));
    }

    progress.step("Repairing bootstrap package dependencies");
    run_bootstrap_command(
        prefix,
        "bootstrap dependency repair",
        &apt_get,
        &["--fix-broken", "install", "-y"],
    )?;

    fs::write(&marker, b"ok\n")
        .with_context(|| format!("write dependency repair marker at {}", marker.display()))?;
    progress.step("Bootstrap package dependencies repaired");
    log::info!("bootstrap_install: package dependency repair completed");
    Ok(())
}

/// Resolve the latest-release tag without naming any asset.
///
/// Hits `https://github.com/<repo>/releases/latest` with redirects
/// disabled. GitHub returns a 302 whose `Location` is
/// `https://github.com/<repo>/releases/tag/<tag>`. Parse the tag out.
/// One HTTP request, no `api.github.com`, no asset name dependency —
/// works even when the release has zero assets uploaded.
fn resolve_latest_tag(release_repo: &str) -> Result<String> {
    let url = format!("https://github.com/{release_repo}/releases/latest");
    let agent_no_redirect = ureq::builder().redirects(0).build();
    let head = agent_no_redirect
        .get(&url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call();
    let resp = match head {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => return Err(anyhow!("HTTP GET {url}: {e}")),
    };
    let location = resp
        .header("Location")
        .ok_or_else(|| anyhow!("no Location header on {url}; got status {}", resp.status()))?;
    let marker = "/releases/tag/";
    let after = location.find(marker).map(|i| &location[i + marker.len()..]);
    let tag = after
        .and_then(|s| s.split('/').next().filter(|t| !t.is_empty()))
        .ok_or_else(|| anyhow!("expected `/releases/tag/<tag>` in Location {location}"))?;
    Ok(tag.to_owned())
}

/// Download the bootstrap zip for the given tag.
///
/// Fast path: try the canonical `RELEASE_ASSET_NAME` under
/// `/<repo>/releases/download/<tag>/<file>`. 200 means we're done,
/// neither request touched `api.github.com`.
///
/// Slow path (on 404): enumerate the release's assets via
/// `api.github.com/repos/<repo>/releases/tags/<tag>` and pick the
/// first asset whose name matches `<ASSET_NAME_PREFIX>*<ASSET_NAME_SUFFIX>`.
/// One API request, eats one quota slot from the 60-req/hour limit,
/// but kicks in only when uploads landed under a non-canonical name.
fn download_bootstrap_asset(
    release_repo: &str,
    tag: &str,
    progress: &mut dyn ProgressSink,
) -> Result<Vec<u8>> {
    let canonical_url =
        format!("https://github.com/{release_repo}/releases/download/{tag}/{RELEASE_ASSET_NAME}");
    match fetch_asset_bytes(&canonical_url, progress, 0) {
        Ok(bytes) => {
            let asset = find_release_asset(release_repo, tag)?;
            validate_bootstrap_asset(&bytes, &asset)?;
            log::info!("bootstrap_install: fetched canonical asset {RELEASE_ASSET_NAME}");
            Ok(bytes)
        }
        Err(FetchError::NotFound) => {
            log::info!(
                "bootstrap_install: canonical {RELEASE_ASSET_NAME} 404'd on tag {tag}; \
                 falling back to API asset enumeration"
            );
            let asset = find_release_asset(release_repo, tag)?;
            if asset.size > MAX_ARCHIVE_BYTES {
                return Err(anyhow!(
                    "bootstrap asset {} is {} bytes, above the {}-byte safety limit",
                    asset.name,
                    asset.size,
                    MAX_ARCHIVE_BYTES
                ));
            }
            let alt_url = format!(
                "https://github.com/{release_repo}/releases/download/{tag}/{}",
                asset.name
            );
            log::info!("bootstrap_install: fetching alt asset {}", asset.name);
            let bytes = fetch_asset_bytes(&alt_url, progress, asset.size)
                .map_err(FetchError::into_anyhow)?;
            validate_bootstrap_asset(&bytes, &asset)?;
            Ok(bytes)
        }
        Err(FetchError::Other(error)) => Err(error),
    }
}

fn validate_bootstrap_asset(bytes: &[u8], asset: &GitHubAsset) -> Result<()> {
    if bytes.len() as u64 != asset.size {
        return Err(anyhow!(
            "bootstrap asset size mismatch: downloaded {}, GitHub reports {}",
            bytes.len(),
            asset.size
        ));
    }
    let expected = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow!("GitHub did not provide a SHA-256 digest for {}", asset.name))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(anyhow!(
            "bootstrap asset SHA-256 mismatch for {}",
            asset.name
        ));
    }
    log::info!(
        "bootstrap_install: verified {} ({} bytes, sha256:{})",
        asset.name,
        asset.size,
        actual
    );
    Ok(())
}

enum FetchError {
    NotFound,
    Other(anyhow::Error),
}

impl FetchError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            FetchError::NotFound => anyhow!("bootstrap asset returned 404"),
            FetchError::Other(error) => error,
        }
    }
}

fn fetch_asset_bytes(
    url: &str,
    progress: &mut dyn ProgressSink,
    expected_size: u64,
) -> std::result::Result<Vec<u8>, FetchError> {
    let resp = ureq::get(url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call();
    let resp = match resp {
        Ok(resp) => resp,
        Err(ureq::Error::Status(404, _)) => return Err(FetchError::NotFound),
        Err(error) => return Err(FetchError::Other(anyhow!("HTTP GET {url}: {error}"))),
    };
    let cap = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    if cap as u64 > MAX_ARCHIVE_BYTES {
        return Err(FetchError::Other(anyhow!(
            "bootstrap response is {cap} bytes, above the {MAX_ARCHIVE_BYTES}-byte safety limit"
        )));
    }
    let total = if cap > 0 {
        cap as u64
    } else {
        expected_size
    };
    let capacity = if expected_size <= usize::MAX as u64 {
        expected_size as usize
    } else {
        cap
    };
    let mut buf = Vec::with_capacity(capacity);
    let mut reader = resp.into_reader();
    let mut chunk = [0u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| FetchError::Other(anyhow!("read body from {url}: {error}")))?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.len() as u64 > MAX_ARCHIVE_BYTES {
            return Err(FetchError::Other(anyhow!(
                "bootstrap response exceeded the {MAX_ARCHIVE_BYTES}-byte safety limit"
            )));
        }
        progress.progress(buf.len() as u64, total);
    }
    Ok(buf)
}

#[derive(Deserialize)]
struct GitHubRelease {
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    digest: Option<String>,
}

/// Resolve the exact release asset and its GitHub-computed digest before
/// downloading. Prefer the canonical name, then accept a versioned variant.
fn find_release_asset(release_repo: &str, tag: &str) -> Result<GitHubAsset> {
    let url = format!("https://api.github.com/repos/{release_repo}/releases/tags/{tag}");
    let release: GitHubRelease = ureq::get(&url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("HTTP GET {url}: {e}"))?
        .into_json()
        .map_err(|e| anyhow!("parse release metadata from {url}: {e}"))?;
    let asset_index = release
        .assets
        .iter()
        .position(|asset| asset.name == RELEASE_ASSET_NAME)
        .or_else(|| {
            release.assets.iter().position(|asset| {
                asset.name.starts_with(ASSET_NAME_PREFIX) && asset.name.ends_with(ASSET_NAME_SUFFIX)
            })
        })
        .ok_or_else(|| {
            anyhow!("release {tag} has no asset matching {ASSET_NAME_PREFIX}*{ASSET_NAME_SUFFIX}")
        })?;
    let mut assets = release.assets;
    Ok(assets.swap_remove(asset_index))
}

fn extract_into_staging(zip_bytes: &[u8], staging: &Path) -> Result<()> {
    if staging.exists() {
        fs::remove_dir_all(staging)
            .with_context(|| format!("wipe leftover staging at {}", staging.display()))?;
    }
    fs::create_dir_all(staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .context("ZipArchive::new on downloaded bootstrap")?;

    let symlinks = extract_entries(&mut archive, staging)?;
    log::info!(
        "bootstrap_install: extracted {} entries, {} symlinks queued",
        archive.len(),
        symlinks.len(),
    );
    replay_symlinks(staging, &symlinks)?;
    Ok(())
}

fn swap_staging_into_prefix(staging: &Path, prefix: &Path) -> Result<()> {
    if prefix.exists() {
        fs::remove_dir_all(prefix)
            .with_context(|| format!("wipe old prefix at {}", prefix.display()))?;
    }
    fs::rename(staging, prefix)
        .with_context(|| format!("rename {} -> {}", staging.display(), prefix.display()))?;
    Ok(())
}

fn extract_entries<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    staging: &Path,
) -> Result<Vec<(String, String)>> {
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(anyhow!(
            "bootstrap archive has {} entries, above the {}-entry safety limit",
            archive.len(),
            MAX_ARCHIVE_ENTRIES
        ));
    }
    let mut symlinks = Vec::new();
    let mut extracted_bytes = 0_u64;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_owned();
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or_else(|| anyhow!("bootstrap extracted-size counter overflow"))?;
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err(anyhow!(
                "bootstrap archive expands beyond the {MAX_EXTRACTED_BYTES}-byte safety limit"
            ));
        }

        if raw_name == SYMLINKS_ENTRY {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            for line in text.lines() {
                if line.is_empty() {
                    continue;
                }
                let Some((target, link_rel)) = line.split_once(SYMLINKS_DELIM) else {
                    log::warn!("bootstrap_install: malformed SYMLINKS.txt line: {line:?}");
                    continue;
                };
                symlinks.push((target.to_owned(), link_rel.to_owned()));
            }
            continue;
        }

        let Some(safe) = entry.enclosed_name() else {
            log::warn!("bootstrap_install: skipping unsafe entry path {raw_name:?}");
            continue;
        };
        let dest: PathBuf = staging.join(&safe);

        if entry.is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }

        if entry.is_symlink() {
            log::warn!(
                "bootstrap_install: unexpected inline symlink entry {raw_name:?}; \
                 skipping (symlinks come via SYMLINKS.txt)"
            );
            continue;
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let entry_mode = entry.unix_mode();
        let mut out =
            fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;

        if let Some(mode) = entry_mode {
            let owner_only = (mode & 0o700) | if mode & 0o100 != 0 { 0o700 } else { 0o600 };
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(owner_only);
            fs::set_permissions(&dest, perms)?;
        } else if raw_name.starts_with("bin/")
            || raw_name.starts_with("libexec/")
            || raw_name.starts_with("lib/apt/methods/")
            || raw_name == "lib/apt/apt-helper"
        {
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(0o700);
            fs::set_permissions(&dest, perms)?;
        }
    }

    Ok(symlinks)
}

fn replay_symlinks(staging: &Path, symlinks: &[(String, String)]) -> Result<()> {
    for (target, link_rel) in symlinks {
        let link_rel = safe_symlink_path(link_rel)?;
        reject_symlink_ancestors(staging, &link_rel)?;
        let link_abs = staging.join(&link_rel);
        if let Some(parent) = link_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        if link_abs.exists() || link_abs.symlink_metadata().is_ok() {
            fs::remove_file(&link_abs).ok();
        }
        std::os::unix::fs::symlink(target, &link_abs)
            .with_context(|| format!("symlink {} -> {}", link_abs.display(), target))?;
    }
    Ok(())
}

fn safe_symlink_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    let has_normal_component = path
        .components()
        .any(|component| matches!(component, Component::Normal(_)));
    if path.as_os_str().is_empty()
        || !has_normal_component
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(anyhow!("unsafe bootstrap symlink path {raw:?}"));
    }
    Ok(path.to_path_buf())
}

fn reject_symlink_ancestors(staging: &Path, relative: &Path) -> Result<()> {
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = staging.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "bootstrap symlink path traverses another symlink at {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(anyhow!(
                    "bootstrap symlink parent is not a directory: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("create symlink parent {}", current.display()))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// `ProgressSink` impl that routes progress events to logcat. Useful
/// when the caller is happy with log-only feedback (e.g. headless
/// first-boot install before any UI is up to render a progress bar).
#[derive(Debug)]
pub struct LogProgressSink;

impl ProgressSink for LogProgressSink {
    fn step(&mut self, label: &str) {
        log::info!("bootstrap_install: {label}");
    }
    fn progress(&mut self, done: u64, total: u64) {
        if total > 0 {
            log::debug!("bootstrap_install: {done}/{total}");
        }
    }
    fn warn(&mut self, message: &str) {
        log::warn!("bootstrap_install: {message}");
    }
}
