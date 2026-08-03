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
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

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
    match fetch_asset_bytes(&canonical_url, progress) {
        Ok(bytes) => {
            log::info!("bootstrap_install: fetched canonical asset {RELEASE_ASSET_NAME}");
            Ok(bytes)
        }
        Err(FetchError::NotFound) => {
            log::info!(
                "bootstrap_install: canonical {RELEASE_ASSET_NAME} 404'd on tag {tag}; \
                 falling back to API asset enumeration"
            );
            let alt_name = find_alt_asset_name(release_repo, tag)?;
            let alt_url =
                format!("https://github.com/{release_repo}/releases/download/{tag}/{alt_name}");
            log::info!("bootstrap_install: fetching alt asset {alt_name}");
            match fetch_asset_bytes(&alt_url, progress) {
                Ok(bytes) => Ok(bytes),
                Err(FetchError::NotFound) => Err(anyhow!(
                    "asset {alt_name} present in API listing but returned 404 on download"
                )),
                Err(FetchError::Other(e)) => Err(e),
            }
        }
        Err(FetchError::Other(e)) => Err(e),
    }
}

enum FetchError {
    NotFound,
    Other(anyhow::Error),
}

fn fetch_asset_bytes(
    url: &str,
    progress: &mut dyn ProgressSink,
) -> std::result::Result<Vec<u8>, FetchError> {
    let resp = ureq::get(url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .call();
    let resp = match resp {
        Ok(resp) => resp,
        Err(ureq::Error::Status(404, _)) => return Err(FetchError::NotFound),
        Err(e) => return Err(FetchError::Other(anyhow!("HTTP GET {url}: {e}"))),
    };
    let cap = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let total = cap as u64;
    let mut buf = Vec::with_capacity(cap);
    let mut reader = resp.into_reader();
    let mut chunk = [0u8; 128 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|e| FetchError::Other(anyhow!("read body from {url}: {e}")))?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        progress.progress(buf.len() as u64, total);
    }
    Ok(buf)
}

/// Enumerate the release's assets via the GitHub API and return the
/// first asset name matching `<ASSET_NAME_PREFIX>*<ASSET_NAME_SUFFIX>`.
/// Used as a fallback when the canonical asset name 404s on download.
fn find_alt_asset_name(release_repo: &str, tag: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{release_repo}/releases/tags/{tag}");
    let body = ureq::get(&url)
        .set("User-Agent", "zdroid-bootstrap-installer")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| anyhow!("HTTP GET {url}: {e}"))?
        .into_string()
        .map_err(|e| anyhow!("read body from {url}: {e}"))?;
    let candidates = parse_asset_names(&body);
    candidates
        .into_iter()
        .find(|name| name.starts_with(ASSET_NAME_PREFIX) && name.ends_with(ASSET_NAME_SUFFIX))
        .ok_or_else(|| {
            anyhow!("release {tag} has no asset matching {ASSET_NAME_PREFIX}*{ASSET_NAME_SUFFIX}")
        })
}

/// Walk the API JSON for `"name": "..."` strings under the `assets`
/// array. Lightweight scrape: avoids pulling serde_json back as a
/// dep for one fallback path that only fires on misnamed uploads.
fn parse_asset_names(body: &str) -> Vec<String> {
    let Some(assets_start) = body.find("\"assets\"") else {
        return Vec::new();
    };
    let tail = &body[assets_start..];
    let mut names = Vec::new();
    let needle = "\"name\":";
    let mut search = tail;
    while let Some(idx) = search.find(needle) {
        search = &search[idx + needle.len()..];
        if let Some(quote_start) = search.find('"') {
            let after_quote = &search[quote_start + 1..];
            if let Some(quote_end) = after_quote.find('"') {
                let name = &after_quote[..quote_end];
                names.push(name.to_owned());
                search = &after_quote[quote_end + 1..];
            }
        }
    }
    names
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
    let mut symlinks = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_owned();

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
        let link_rel = link_rel.trim_start_matches("./");
        let link_abs = staging.join(link_rel);
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
