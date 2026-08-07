#![cfg(target_os = "android")]
//! Zed Workspace running on Android. Boots up the full client/project/
//! workspace stack and shows the WelcomePage on first launch (no auto-
//! opened project) — matches official Zed's first-run behaviour.

mod header;
mod menu_bar;
mod noexec_modal;
mod runtime_picker;
mod title_bar;

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use android_activity::AndroidApp;
use anyhow::{Context as _, Result};
use client::{Client, UserStore};
use db::AppDatabase;
use db::kvp::KeyValueStore;
use fs::{Fs, RealFs};
use gpui::{App, AppContext as _, TaskExt as _, UpdateGlobal as _};
use log::{error, info};
use node_runtime::NodeRuntime;
use project::Project;
use prompt_store::PromptBuilder;
use reqwest_client::ReqwestClient;
use session::{AppSession, Session};
use settings::{Settings as _, SettingsStore};
use util::ResultExt as _;
use workspace::{
    AppState, CloseIntent, CloseProject, MultiWorkspace, OpenOptions, SerializedWorkspaceLocation,
    SessionWorkspace, Workspace, WorkspaceDb, WorkspaceStore, open_new,
    workspace_windows_for_location,
};
use zdroid_runtime::{
    RuntimeId, RuntimeProvider, adapters,
    config::{ResolvedConfig, RuntimeFile},
};

#[derive(Clone, Copy)]
struct AndroidDnsResolver {
    vm_ptr: usize,
}

impl AndroidDnsResolver {
    fn new(android_app: &AndroidApp) -> Self {
        Self {
            vm_ptr: android_app.vm_as_ptr() as usize,
        }
    }

    fn resolve_now(&self, hostname: &str) -> Result<Vec<SocketAddr>> {
        use jni::JavaVM;
        use jni::objects::{JObject, JObjectArray, JString, JValue};

        let vm = unsafe { JavaVM::from_raw(self.vm_ptr as *mut _)? };
        let mut env = vm
            .attach_current_thread()
            .context("attach thread for Android DNS")?;
        let host = env.new_string(hostname).context("create DNS hostname")?;
        let result = env
            .call_static_method(
                "java/net/InetAddress",
                "getAllByName",
                "(Ljava/lang/String;)[Ljava/net/InetAddress;",
                &[JValue::Object(&JObject::from(host))],
            )
            .context("InetAddress.getAllByName")?
            .l()
            .context("DNS result is not an object")?;
        let addresses = JObjectArray::from(result);
        let length = env
            .get_array_length(&addresses)
            .context("DNS result length")?;
        let mut resolved = Vec::with_capacity(length as usize);
        for index in 0..length {
            let address = env
                .get_object_array_element(&addresses, index)
                .context("read DNS result")?;
            let value = env
                .call_method(&address, "getHostAddress", "()Ljava/lang/String;", &[])
                .context("InetAddress.getHostAddress")?
                .l()
                .context("DNS address is not a string")?;
            let value: String = env
                .get_string(&JString::from(value))
                .context("decode DNS address")?
                .into();
            if let Ok(ip) = value.parse::<IpAddr>() {
                resolved.push(SocketAddr::new(ip, 0));
            }
        }
        anyhow::ensure!(
            !resolved.is_empty(),
            "Android returned no addresses for {hostname}"
        );
        Ok(resolved)
    }
}

impl reqwest::dns::Resolve for AndroidDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let result = self
            .resolve_now(name.as_str())
            .map(|addresses| Box::new(addresses.into_iter()) as reqwest::dns::Addrs);
        Box::pin(std::future::ready(result.map_err(Into::into)))
    }
}

fn minimal_window_options(_: Option<uuid::Uuid>, _cx: &mut App) -> gpui::WindowOptions {
    gpui::WindowOptions::default()
}

fn show_open_project_error(
    multi_workspace: Option<&gpui::WindowHandle<MultiWorkspace>>,
    workspace: Option<&gpui::WindowHandle<Workspace>>,
    message: &str,
    cx: &mut gpui::AsyncApp,
) {
    if let Some(multi_workspace) = multi_workspace {
        let _ = multi_workspace.update(cx, |_, window, cx| {
            window.prompt(
                gpui::PromptLevel::Warning,
                "Could not open project",
                Some(message),
                &["OK"],
                cx,
            )
        });
    } else if let Some(workspace) = workspace {
        let _ = workspace.update(cx, |_, window, cx| {
            window.prompt(
                gpui::PromptLevel::Warning,
                "Could not open project",
                Some(message),
                &["OK"],
                cx,
            )
        });
    }
}

/// Build the active adapter from `runtime.toml`, returning the boxed
/// provider so callers can ask for its `environment_root()` and
/// adapter-derived metadata. Returns None when no toml exists or the
/// adapter construction fails (defaults are filled in by the picker
/// the first time the user opens it).
fn build_active_provider(data_path: &std::path::Path) -> Option<Box<dyn RuntimeProvider>> {
    let runtime_path = data_path.join("usr/etc/zd-runtime.toml");
    let file = RuntimeFile::load(&runtime_path).ok().flatten()?;
    let mut resolved = file.resolve().ok()?;
    if matches!(&resolved, ResolvedConfig::ExternalTermux(_)) {
        log::warn!(
            "zed_android: External Termux was selected, but its interactive stdio bridge is not implemented; using Bootstrap"
        );
        let fallback = RuntimeFile::with_defaults(RuntimeId::Bootstrap);
        if let Err(err) = fallback.save(&runtime_path) {
            log::warn!(
                "zed_android: could not repair unsupported External Termux selection at {}: {err:#}",
                runtime_path.display()
            );
        }
        resolved = fallback.resolve().ok()?;
    }
    adapters::for_config(&resolved).ok()
}

/// Same as `build_active_provider` but never returns `None`. When no
/// runtime.toml exists yet (first launch before the picker is opened)
/// we fall back to a default Bootstrap adapter so the env-init path
/// always has someone to ask. The user's eventual picker selection
/// rewrites runtime.toml and the next launch picks up the chosen
/// adapter via `build_active_provider`.
fn active_provider(data_path: &std::path::Path) -> Box<dyn RuntimeProvider> {
    if let Some(provider) = build_active_provider(data_path) {
        return provider;
    }
    let file = RuntimeFile::with_defaults(RuntimeId::Bootstrap);
    let resolved = file
        .resolve()
        .expect("default Bootstrap RuntimeFile must resolve");
    adapters::for_config(&resolved).expect("default Bootstrap adapter must construct")
}

const CODEX_TERMUX_VERSION: &str = "0.146.0";

fn zdroid_bootstrap_paths() -> Result<(PathBuf, PathBuf)> {
    let prefix = std::env::var_os("PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/data/com.zdroid/files/usr"));
    let home = prefix
        .parent()
        .context("Zdroid bootstrap prefix has no parent")?
        .join("home");
    std::fs::create_dir_all(&home).context("create Zdroid terminal home")?;
    Ok((prefix, home))
}

/// Make the subscription-backed ACP agents available without asking users to
/// configure an API provider. Both adapters inherit the active runtime's HOME,
/// so they reuse the login performed by `codex login` / `claude` in Zdroid's
/// integrated terminal. Codex must use an Android-targeted binary: the regular
/// Linux-musl npm binary cannot reliably use Android's netd resolver.
fn ensure_codex_acp_launcher() -> Result<PathBuf> {
    let (prefix, home) = zdroid_bootstrap_paths()?;
    let launcher = home.join(".local/bin/zdroid-codex-acp-cli");
    let parent = launcher.parent().context("Codex launcher has no parent")?;
    std::fs::create_dir_all(parent).context("create Codex launcher directory")?;

    let browser_launcher = home.join(".local/bin/zdroid-open-url");
    std::fs::write(
        &browser_launcher,
        r#"#!/system/bin/sh
set -eu
url="${1:-}"
if [ -z "$url" ]; then exit 2; fi
/system/bin/log -t zed_android_browser "Opening OAuth URL through Zdroid bridge" || true
exec /system/bin/am broadcast \
  -a com.zdroid.action.OPEN_URL \
  -n com.zdroid/.OpenUrlReceiver \
  --es url "$url"
"#,
    )
    .context("write Android browser bridge")?;
    let mut permissions = std::fs::metadata(&browser_launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&browser_launcher, permissions)
        .context("chmod Android browser bridge")?;

    // Rust's `open` crate prefers Termux's helper on Android and can ignore
    // BROWSER entirely. Shadow it in the managed PATH so Codex never targets
    // com.termux/.app.TermuxOpenReceiver from inside the Zdroid sandbox.
    let termux_open_url_launcher = home.join(".local/bin/termux-open-url");
    std::fs::write(
        &termux_open_url_launcher,
        format!(
            "#!/system/bin/sh\n/system/bin/log -t zed_android_browser 'Redirecting termux-open-url through Zdroid' || true\nexec {} \"$@\"\n",
            browser_launcher.to_string_lossy()
        ),
    )
    .context("write Termux URL compatibility bridge")?;
    let mut permissions = std::fs::metadata(&termux_open_url_launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&termux_open_url_launcher, permissions)
        .context("chmod Termux URL compatibility bridge")?;

    let script = format!(
        r#"#!/system/bin/sh
set -eu

version="{CODEX_TERMUX_VERSION}"
prefix="{prefix}"
root="$HOME/.local/share/zdroid/codex-termux-$version"
codex_bin="$root/node_modules/@mmmbuto/codex-cli-termux/bin/codex.bin"
archive="/sdcard/.zed/mmmbuto-codex-cli-termux-$version.tgz"
mkdir -p "$HOME/.local/share/zdroid"

if [ ! -x "$codex_bin" ]; then
    lock="$root.installing"
    if [ -d "$lock" ]; then
        owner="$(cat "$lock/pid" 2>/dev/null || true)"
        owner_start="$(cat "$lock/start_time" 2>/dev/null || true)"
        live_start=""
        if [ -n "$owner" ] && [ -r "/proc/$owner/stat" ]; then
            live_start="$(awk '{{print $22}}' "/proc/$owner/stat" 2>/dev/null || true)"
        fi
        if [ -z "$owner_start" ] || [ "$owner_start" != "$live_start" ]; then
            rm -rf "$lock"
        fi
    fi
    if mkdir "$lock" 2>/dev/null; then
        echo "$$" > "$lock/pid"
        awk '{{print $22}}' "/proc/$$/stat" > "$lock/start_time"
        trap 'rm -rf "$lock" "$root.staging"' EXIT INT TERM
        rm -rf "$root.staging"
        mkdir -p "$root.staging"
        echo "Zdroid-B: installing Android Codex $version (one-time setup)..." >&2
        if [ -f "$archive" ]; then source="$archive"; else source="@mmmbuto/codex-cli-termux@$version"; fi
        npm_config_platform=android "$prefix/bin/npm" install --force --no-audit --no-fund --prefix "$root.staging" "$source" >&2
        rm -rf "$root"
        mv "$root.staging" "$root"
        rm -rf "$lock"
        trap - EXIT INT TERM
    else
        waited=0
        while [ ! -x "$codex_bin" ] && [ "$waited" -lt 900 ]; do
            sleep 1
            waited=$((waited + 1))
        done
    fi
fi

if [ ! -x "$codex_bin" ]; then
    echo "Zdroid-B: Android Codex installation did not complete." >&2
    exit 127
fi

# The Android package is downloaded after app startup, so the boot-time repair
# cannot see it. Patch the resolver path immediately before every launch. The
# replacement has the same 16-byte width and fd 9 is opened below.
"$prefix/bin/node" -e '
const fs = require("fs");
const path = process.argv[1];
const replacement = Buffer.from("/proc/self/fd/9\0", "binary");
let bytes = fs.readFileSync(path);
let changed = false;
for (const value of ["/etc/resolv.conf", "/sdcard/.zed/r\0\0"]) {{
    const needle = Buffer.from(value, "binary");
    for (let offset = bytes.indexOf(needle); offset >= 0; offset = bytes.indexOf(needle, offset + replacement.length)) {{
        replacement.copy(bytes, offset);
        changed = true;
    }}
}}
if (changed) fs.writeFileSync(path, bytes);
' "$codex_bin"

bin_dir="$(dirname "$codex_bin")"
resolv_conf="${{ZDROID_RESOLV_CONF:-$HOME/../zdroid-resolv.conf}}"
if [ ! -r "$resolv_conf" ]; then
    printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' > "$resolv_conf"
fi
exec 9<"$resolv_conf"
export CODEX_MANAGED_BY_NPM=1
export CODEX_SELF_EXE="$codex_bin"
export BROWSER="{browser_launcher}"
export PATH="{managed_bin}:$PATH"
export LD_LIBRARY_PATH="$bin_dir${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}"
exec "$codex_bin" "$@"
"#,
        browser_launcher = browser_launcher.to_string_lossy(),
        managed_bin = parent.to_string_lossy(),
        prefix = prefix.to_string_lossy(),
    );
    std::fs::write(&launcher, script).context("write Codex ACP launcher")?;
    let mut permissions = std::fs::metadata(&launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&launcher, permissions).context("chmod Codex ACP launcher")?;
    let terminal_launcher = prefix.join(".zed/bin/codex");
    if let Some(parent) = terminal_launcher.parent() {
        std::fs::create_dir_all(parent).context("create Codex terminal launcher directory")?;
    }
    let terminal_script = format!(
        "#!/system/bin/sh\nexec {} \"$@\"\n",
        launcher.to_string_lossy()
    );
    std::fs::write(&terminal_launcher, terminal_script).context("write Codex terminal launcher")?;
    let mut permissions = std::fs::metadata(&terminal_launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&terminal_launcher, permissions)
        .context("chmod Codex terminal launcher")?;

    Ok(launcher)
}

fn ensure_claude_acp_launcher() -> Result<PathBuf> {
    let (prefix, home) = zdroid_bootstrap_paths()?;
    if let Err(err) = ensure_claude_model_catalog(&home) {
        log::warn!("zed_android: could not update Claude model catalog: {err:#}");
    }
    let launcher = home.join(".local/bin/zdroid-claude-agent-acp");
    let parent = launcher
        .parent()
        .context("Claude ACP launcher has no parent")?;
    std::fs::create_dir_all(parent).context("create Claude ACP launcher directory")?;

    // This release asks the Claude SDK for the account's live model list and
    // exposes full model versions through ACP. Pin it so a registry cache from
    // an older Zdroid install cannot silently fall back to family aliases only.
    let script = r#"#!/system/bin/sh
set -eu
claude_cli="$PREFIX/.zed/bin/claude"
if [ ! -x "$claude_cli" ]; then
    echo 'Zdroid-B: Claude launcher is missing. Restart Zdroid-B, then use Install Claude in Agent Info.' >&2
    exit 127
fi
export CLAUDE_CODE_EXECUTABLE="$claude_cli"
export DISABLE_AUTOUPDATER=1
cache="${npm_config_cache:-$HOME/../node/cache}"
managed_acp="$HOME/.local/share/zdroid/claude-code/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"
managed_package="$(dirname "$(dirname "$managed_acp")")/package.json"
managed_version="$(node -p "try { require('$managed_package').version } catch (_) { '' }" 2>/dev/null || true)"
if [ -f "$managed_acp" ] && [ "$managed_version" = "0.64.2" ]; then
    /system/bin/log -t zed_android "Claude ACP source=managed version=$managed_version" || true
    exec node "$managed_acp" "$@"
fi

# Always resolve the pinned package when the managed copy is missing or stale.
# A hash directory in npm's _npx cache does not encode package version, so
# blindly running the last matching file can silently revive an older ACP.
/system/bin/log -t zed_android "Claude ACP source=npx version=0.64.2 managed_version=${managed_version:-missing}" || true
exec npx --yes --prefer-offline --no-audit --no-fund @agentclientprotocol/claude-agent-acp@0.64.2 "$@"
"#;
    std::fs::write(&launcher, script).context("write Claude ACP launcher")?;
    let mut permissions = std::fs::metadata(&launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&launcher, permissions).context("chmod Claude ACP launcher")?;

    // The registry-provided launch command is `claude-agent-acp`. Settings are
    // persisted asynchronously, so on a fresh boot AgentRegistryStore can see
    // that command before it sees our Custom-server replacement. Keep a small
    // compatibility entry on the bootstrap PATH so either launch route reaches
    // the same Android-aware wrapper instead of failing with status 127.
    let terminal_launcher = prefix.join(".zed/bin/claude");
    if let Some(parent) = terminal_launcher.parent() {
        std::fs::create_dir_all(parent).context("create Claude terminal launcher directory")?;
    }
    std::fs::write(
        &terminal_launcher,
        r#"#!/system/bin/sh
set -eu
managed_cli="$HOME/.local/share/zdroid/claude-code/node_modules/@anthropic-ai/claude-code/cli.js"
global_cli="$PREFIX/lib/node_modules/@anthropic-ai/claude-code/cli.js"
if [ -f "$managed_cli" ]; then
    resolved_cli="$managed_cli"
elif [ -f "$global_cli" ]; then
    resolved_cli="$global_cli"
else
    real_cli="$PREFIX/bin/claude"
    resolved_cli="$(readlink -f "$real_cli" 2>/dev/null || true)"
fi
if [ -z "${resolved_cli:-}" ] || [ ! -f "$resolved_cli" ]; then
    echo 'Zdroid-B: A compatible Claude CLI is not installed. Use Install Claude in Agent Info; unversioned npm releases from 2.1.113 onward are native binaries that cannot run in Android/Bionic.' >&2
    exit 127
fi
case "$resolved_cli" in
    *.exe)
        echo 'Zdroid-B: This Claude CLI uses an incompatible native executable. Use Install Claude in Agent Info to install the Android-compatible JavaScript release.' >&2
        exit 126
        ;;
esac
resolv_conf="${ZDROID_RESOLV_CONF:-$HOME/../zdroid-resolv.conf}"
if [ ! -r "$resolv_conf" ]; then
    printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' > "$resolv_conf"
fi
exec 9<"$resolv_conf"
# Keep the pinned JavaScript release in app-owned HOME. This avoids npm global
# shims disappearing during package upgrades and prevents Claude's updater
# from migrating to an Android-incompatible native build.
export DISABLE_AUTOUPDATER=1
export USE_BUILTIN_RIPGREP=0
exec node "$resolved_cli" "$@"
"#,
    )
    .context("write Claude terminal launcher")?;
    let mut permissions = std::fs::metadata(&terminal_launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&terminal_launcher, permissions)
        .context("chmod Claude terminal launcher")?;

    let compatibility_launcher = prefix.join("bin/claude-agent-acp");
    if let Some(parent) = compatibility_launcher.parent() {
        std::fs::create_dir_all(parent).context("create Claude ACP compatibility directory")?;
    }
    let compatibility_script = format!(
        "#!/system/bin/sh\nexec {} \"$@\"\n",
        launcher.to_string_lossy()
    );
    std::fs::write(&compatibility_launcher, compatibility_script)
        .context("write Claude ACP compatibility launcher")?;
    let mut permissions = std::fs::metadata(&compatibility_launcher)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&compatibility_launcher, permissions)
        .context("chmod Claude ACP compatibility launcher")?;
    log::info!(
        "zed_android: Claude ACP compatibility launcher = {}",
        compatibility_launcher.display()
    );
    Ok(launcher)
}

/// Recreate launchers that live inside `$PREFIX` after Bootstrap atomically
/// replaces that directory on a first install or runtime upgrade.
pub(crate) fn ensure_agent_cli_launchers() -> Result<()> {
    ensure_codex_acp_launcher().context("recreate Codex launchers")?;
    ensure_claude_acp_launcher().context("recreate Claude launchers")?;
    Ok(())
}

const ZDROID_CLAUDE_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-sonnet-5",
    "claude-sonnet-4-7",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

fn ensure_claude_model_catalog(home: &Path) -> Result<()> {
    let settings_dir = home.join(".claude");
    let settings_path = settings_dir.join("settings.json");
    std::fs::create_dir_all(&settings_dir).context("create Claude settings directory")?;

    let mut settings = if settings_path.is_file() {
        let bytes = std::fs::read(&settings_path).context("read Claude settings")?;
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .context("parse Claude settings as JSON")?
    } else {
        serde_json::json!({})
    };
    let object = settings
        .as_object_mut()
        .context("Claude settings root is not a JSON object")?;
    let models = object
        .entry("availableModels")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .context("Claude availableModels is not a JSON array")?;

    let mut changed = false;
    for model in ZDROID_CLAUDE_MODELS {
        if !models.iter().any(|value| value.as_str() == Some(model)) {
            models.push(serde_json::Value::String((*model).to_string()));
            changed = true;
        }
    }
    if !changed && settings_path.is_file() {
        return Ok(());
    }

    let temporary_path = settings_dir.join("settings.json.zdroid.tmp");
    std::fs::write(&temporary_path, serde_json::to_vec_pretty(&settings)?)
        .context("write temporary Claude settings")?;
    std::fs::rename(&temporary_path, &settings_path).context("replace Claude settings")?;
    log::info!(
        "zed_android: ensured {} Claude model catalog entries",
        ZDROID_CLAUDE_MODELS.len()
    );
    Ok(())
}

const RESOLV_CONF_NEEDLE: &[u8; 16] = b"/etc/resolv.conf";
const LEGACY_RESOLV_CONF_NEEDLE: &[u8; 16] = b"/sdcard/.zed/r\0\0";
const RESOLV_CONF_REPLACEMENT: &[u8; 16] = b"/proc/self/fd/9\0";

fn patch_native_cli_binary(path: &Path) -> Result<bool> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() < 1_048_576 {
        return Ok(false);
    }

    let mut bytes = std::fs::read(path)?;
    let mut patched = false;
    for needle in [RESOLV_CONF_NEEDLE, LEGACY_RESOLV_CONF_NEEDLE] {
        let mut offset = 0;
        while let Some(relative) = bytes[offset..]
            .windows(needle.len())
            .position(|window| window == needle)
        {
            let start = offset + relative;
            bytes[start..start + RESOLV_CONF_REPLACEMENT.len()]
                .copy_from_slice(RESOLV_CONF_REPLACEMENT);
            patched = true;
            offset = start + RESOLV_CONF_REPLACEMENT.len();
        }
    }
    if patched {
        std::fs::write(path, bytes)?;
    }
    Ok(patched)
}

fn repair_installed_cli_dns() {
    fn visit(path: &Path, depth: usize, patched: &mut usize) {
        if depth == 0 || !path.exists() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, depth - 1, patched);
                continue;
            }
            let is_cli_binary = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "codex" || name == "claude");
            if is_cli_binary {
                match patch_native_cli_binary(&path) {
                    Ok(true) => {
                        *patched += 1;
                        log::info!("zed_android: repaired CLI DNS path in {}", path.display());
                    }
                    Ok(false) => {}
                    Err(err) => log::warn!(
                        "zed_android: could not repair CLI DNS path in {}: {err:#}",
                        path.display()
                    ),
                }
            }
        }
    }

    let Some(prefix) = std::env::var_os("PREFIX").map(PathBuf::from) else {
        return;
    };
    let mut patched = 0;
    visit(&prefix.join("lib/node_modules"), 14, &mut patched);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        visit(&home.join(".local/share/zdroid"), 16, &mut patched);
    }
    log::info!("zed_android: native CLI DNS repair patched {patched} binaries");
}

struct AndroidAcpAgent {
    id: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

const ANDROID_ACP_AGENTS: &[AndroidAcpAgent] = &[
    AndroidAcpAgent {
        id: "gemini",
        command: "gemini",
        args: &["--acp"],
    },
    AndroidAcpAgent {
        id: "github-copilot-cli",
        command: "copilot",
        args: &["--acp", "--stdio"],
    },
    AndroidAcpAgent {
        id: "grok-build",
        command: "grok",
        args: &["agent", "stdio"],
    },
    AndroidAcpAgent {
        id: "opencode",
        command: "opencode",
        args: &["acp"],
    },
];

fn executable_on_path(command: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(command);
        candidate.is_file().then_some(candidate)
    })
}

fn default_registry_agent_settings() -> settings::CustomAgentServerSettings {
    settings::CustomAgentServerSettings::Registry {
        env: HashMap::default(),
        default_mode: None,
        default_model: None,
        favorite_models: Vec::new(),
        default_config_options: HashMap::default(),
        favorite_config_option_values: HashMap::default(),
    }
}

fn configure_android_acp_agent(
    agent_servers: &mut settings::AllAgentServersSettings,
    spec: &AndroidAcpAgent,
) {
    let executable = executable_on_path(spec.command);

    if let Some(executable) = executable {
        let existing = agent_servers.get(spec.id).cloned();
        let replacement = match existing {
            Some(settings::CustomAgentServerSettings::Custom { .. }) => None,
            Some(settings::CustomAgentServerSettings::Registry {
                env,
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            }) => Some(settings::CustomAgentServerSettings::Custom {
                path: executable.clone(),
                args: spec.args.iter().map(|arg| (*arg).to_string()).collect(),
                env,
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            }),
            None => Some(settings::CustomAgentServerSettings::Custom {
                path: executable.clone(),
                args: spec.args.iter().map(|arg| (*arg).to_string()).collect(),
                env: HashMap::default(),
                default_mode: None,
                default_model: None,
                favorite_models: Vec::new(),
                default_config_options: HashMap::default(),
                favorite_config_option_values: HashMap::default(),
            }),
        };
        if let Some(replacement) = replacement {
            agent_servers.insert(spec.id.to_string(), replacement);
        }
        log::info!(
            "zed_android: ACP agent {} uses terminal CLI {}",
            spec.id,
            executable.display()
        );
    } else {
        agent_servers
            .entry(spec.id.to_string())
            .or_insert_with(default_registry_agent_settings);
        log::info!(
            "zed_android: ACP agent {} uses registry fallback; terminal command {} was not found",
            spec.id,
            spec.command
        );
    }
}

fn ensure_cli_subscription_agents(fs: Arc<dyn Fs>, cx: &mut App) {
    let codex_launcher = match ensure_codex_acp_launcher() {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(err) => {
            log::error!("zed_android: failed to install Codex ACP launcher: {err:#}");
            None
        }
    };
    let claude_launcher = match ensure_claude_acp_launcher() {
        Ok(path) => Some(path),
        Err(err) => {
            log::error!("zed_android: failed to install Claude ACP launcher: {err:#}");
            None
        }
    };

    // Apply the launch contract synchronously before any Project creates its
    // AgentServerStore. Persisting settings below is intentionally async, but
    // relying on that write alone lets a project register the cached Registry
    // command first (`claude-agent-acp`) and fail with status 127 when the user
    // opens a thread quickly. The persisted update contains the same values and
    // replaces this temporary global override once its atomic write completes.
    let mut runtime_settings =
        project::agent_server_store::AllAgentServersSettings::get_global(cx).clone();
    let codex = runtime_settings
        .entry("codex-acp".into())
        .or_insert_with(
            || project::agent_server_store::CustomAgentServerSettings::Registry {
                env: HashMap::default(),
                default_mode: None,
                default_model: None,
                favorite_models: Vec::new(),
                default_config_options: HashMap::default(),
                favorite_config_option_values: HashMap::default(),
            },
        );
    if let project::agent_server_store::CustomAgentServerSettings::Registry { env, .. } = codex {
        if let Some(launcher) = codex_launcher.as_ref() {
            env.insert("CODEX_PATH".to_string(), launcher.clone());
        } else {
            env.remove("CODEX_PATH");
        }
    }

    if let Some(launcher) = claude_launcher.as_ref() {
        let existing = runtime_settings.remove("claude-acp");
        let (
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options,
            favorite_config_option_values,
        ) = match existing {
            Some(project::agent_server_store::CustomAgentServerSettings::Custom {
                command,
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            }) => (
                command.env.unwrap_or_default(),
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            ),
            Some(project::agent_server_store::CustomAgentServerSettings::Registry {
                env,
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            }) => (
                env,
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            ),
            None => (
                HashMap::default(),
                None,
                None,
                Vec::new(),
                HashMap::default(),
                HashMap::default(),
            ),
        };
        let mut env = env;
        env.insert("CLAUDE_CODE_EXECUTABLE".to_string(), "claude".to_string());
        runtime_settings.insert(
            "claude-acp".into(),
            project::agent_server_store::CustomAgentServerSettings::Custom {
                command: project::agent_server_store::AgentServerCommand {
                    path: launcher.clone(),
                    args: Vec::new(),
                    env: Some(env),
                },
                default_mode,
                default_model,
                favorite_models,
                default_config_options,
                favorite_config_option_values,
            },
        );
    }
    project::agent_server_store::AllAgentServersSettings::override_global(runtime_settings, cx);

    cx.global::<SettingsStore>()
        .update_settings_file(fs, move |content, _cx| {
            let agent_servers = content.agent_servers.get_or_insert_default();

            let codex = agent_servers
                .entry("codex-acp".to_string())
                .or_insert_with(|| settings::CustomAgentServerSettings::Registry {
                    env: HashMap::default(),
                    default_mode: None,
                    default_model: None,
                    favorite_models: Vec::new(),
                    default_config_options: HashMap::default(),
                    favorite_config_option_values: HashMap::default(),
                });
            if let settings::CustomAgentServerSettings::Registry { env, .. } = codex {
                if let Some(launcher) = codex_launcher.as_ref() {
                    env.insert("CODEX_PATH".to_string(), launcher.clone());
                } else {
                    env.remove("CODEX_PATH");
                }
            }

            let claude = agent_servers
                .entry("claude-acp".to_string())
                .or_insert_with(|| settings::CustomAgentServerSettings::Registry {
                    env: HashMap::default(),
                    default_mode: None,
                    default_model: None,
                    favorite_models: Vec::new(),
                    default_config_options: HashMap::default(),
                    favorite_config_option_values: HashMap::default(),
                });
            match claude {
                settings::CustomAgentServerSettings::Registry {
                    env,
                    default_mode,
                    default_model,
                    favorite_models,
                    default_config_options,
                    favorite_config_option_values,
                } if claude_launcher.is_some() => {
                    env.entry("CLAUDE_CODE_EXECUTABLE".to_string())
                        .or_insert_with(|| "claude".to_string());
                    *claude = settings::CustomAgentServerSettings::Custom {
                        path: claude_launcher.clone().unwrap(),
                        args: Vec::new(),
                        env: std::mem::take(env),
                        default_mode: default_mode.take(),
                        default_model: default_model.take(),
                        favorite_models: std::mem::take(favorite_models),
                        default_config_options: std::mem::take(default_config_options),
                        favorite_config_option_values: std::mem::take(
                            favorite_config_option_values,
                        ),
                    };
                }
                settings::CustomAgentServerSettings::Custom {
                    path, args, env, ..
                } => {
                    if let Some(launcher) = claude_launcher.as_ref() {
                        *path = launcher.clone();
                        args.clear();
                    }
                    env.entry("CLAUDE_CODE_EXECUTABLE".to_string())
                        .or_insert_with(|| "claude".to_string());
                }
                settings::CustomAgentServerSettings::Registry { env, .. } => {
                    env.entry("CLAUDE_CODE_EXECUTABLE".to_string())
                        .or_insert_with(|| "claude".to_string());
                }
            }

            for spec in ANDROID_ACP_AGENTS {
                configure_android_acp_agent(agent_servers, spec);
            }
        });
}

// Bundled fonts. Upstream Zed walks `assets/fonts/` via `AssetSource`
// at boot (see `load_embedded_fonts` in crates/zed/src/main.rs) and
// hands every `.ttf` to `cx.text_system().add_fonts(...)`. The Android
// example doesn't run that loader, so until this list was filled out
// only Lilex-Regular existed in the text system: bold/italic fell back
// to faux-styled rasterizer output, IBM Plex Sans (the upstream UI
// font) was unavailable, and any `buffer_font_family` setting pointing
// at a different family silently no-op'd.
//
// `include_bytes!` baked into the .so means the fonts ship in the APK
// without going through AAssetManager — they're literally in the .so's
// rodata, so first read is mmap-direct with no extract / decompress
// step. Total budget across the 8 weights is ~3 MB, irrelevant against
// the bundled bootstrap zip.
const BUNDLED_FONTS: &[&[u8]] = &[
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Regular.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Bold.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-Italic.ttf"),
    include_bytes!("../../../../../assets/fonts/lilex/Lilex-BoldItalic.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../../../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf"),
];

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("zed_android")
            // android-activity 0.6.1 hard-codes an `error!` log for
            // "Spurious ALOOPER_POLL_CALLBACK from ALooper_pollOnce()
            // (ignored)" every time the looper dispatches our
            // Choreographer FD callback — which is once per vsync, so
            // 120 lines/sec of logcat noise on Tab S9. The upstream
            // comment says the NDK docs claim this can't happen; it
            // does on real hardware. Silencing the module entirely
            // because nothing else useful comes out of it. If
            // android-activity upgrades and starts surfacing real
            // input/lifecycle errors there, revisit.
            .with_filter(
                android_logger::FilterBuilder::new()
                    .parse("info,android_activity::activity_impl=off")
                    .build(),
            ),
    );
    info!("zed_android: android_main entry");

    let data_path = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/data/com.zdroid/files"));
    info!("zed_android: data_path = {}", data_path.display());

    // The Zed-Rust process env is now adapter-owned. Every set_var/
    // remove_var that used to live inline here is produced by the
    // active runtime adapter's `env_for_zed_process`:
    //
    //   - Chroot adapter ships a bionic-clean env (no PREFIX, no
    //     TERMUX__*, no libtermux-exec preload). PATH front-loaded with
    //     the zd-runtime symlink farm so `Command::new("java")` finds
    //     zd-exec → zd-spawnd → chroot dispatch.
    //
    //   - Bootstrap adapter keeps the historical Termux-flavored env
    //     so dpkg patches, apt Post-Invoke hooks, and bootstrap-side
    //     tooling keep working unchanged.
    //
    //   - External Termux returns a minimal bionic env; spawns route
    //     via Intent (Phase 7) so nothing here leaks across.
    //
    // The active provider also publishes a terminal env overlay that
    // `crates/terminal/src/terminal.rs` applies when the PTY spawns,
    // since alacritty wipes inherited env on bringup.
    //
    // SAFETY: `set_var` / `remove_var` mutate libc-shared process
    // state. The invariant is "no other thread reads/writes libc env
    // via getenv/setenv at this point" — JVM service threads exist by
    // android_main but don't touch libc env. OnceLock makes activity-
    // recreation re-entry a deterministic no-op.
    //
    // DELIBERATELY NOT SET: LD_LIBRARY_PATH. Setting it globally
    // poisons every spawned subprocess (including /system/bin/
    // app_process64 trying to load /system/lib64/libsqlite.so against
    // our OpenSSL 3.x); Upstream Termux packages that need it set it
    // per-spawn in their launcher scripts. ZED_RELEASE_CHANNEL is also
    // deliberately not flipped to "nightly": channel switching
    // namespaces Zed's app data dir and would shadow the user's
    // existing settings. The dev-channel hard-bail in
    // `crates/remote/src/transport/ssh.rs` is patched directly.
    let provider = active_provider(&data_path);
    log::info!("zed_android: runtime adapter = {:?}", provider.id());

    static ENV_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = ENV_INITIALIZED.get_or_init(|| {
        let ops = provider.env_for_zed_process(&data_path);
        unsafe {
            for (key, op) in &ops {
                match op {
                    util::env::EnvOp::Set(value) => std::env::set_var(key, value),
                    util::env::EnvOp::Remove => std::env::remove_var(key),
                }
            }
        }
        log::info!(
            "zed_android: PATH = {}; SHELL = {}",
            std::env::var("PATH").unwrap_or_default(),
            std::env::var("SHELL").unwrap_or_default(),
        );
    });

    // Publish the per-adapter terminal env overlay so
    // `crates/terminal/src/terminal.rs` can apply it when the PTY
    // spawns. Idempotent: subsequent android_main re-entries (activity
    // recreation) get the same overlay; the registration is OnceLock-
    // guarded inside util::env.
    let mut terminal_env = provider.env_for_terminal(&data_path);
    terminal_env.extend(gpui_android::github_credentials::terminal_env(&data_path));
    util::env::register_terminal_env_overlay(terminal_env);

    // Publish adapter-specific filesystem hints for editor code that
    // historically read TERMUX__HOME / TERMUX__PREFIX env vars
    // directly. Now those readers (workspace::welcome,
    // node_runtime::node_runtime, gpui_android::storage) ask the
    // active adapter via these registry slots — Bootstrap publishes
    // its Termux-flavored paths, chroot publishes the bionic-clean
    // equivalents, external Termux publishes None.
    util::env::register_workspace_root(provider.workspace_root(&data_path));
    util::env::register_npm_libtermux_exec_path(provider.npm_libtermux_exec_path(&data_path));

    // Surface the SELinux domain in logcat — this is the canary for the
    // targetSdk pin. If `untrusted_app_27` flips to `untrusted_app_all`
    // every subsequent execve into $PREFIX/bin/* will EACCES.
    gpui_android::termux_bootstrap::check_selinux_context();

    // Best-effort runtime READ/WRITE_EXTERNAL_STORAGE prompt. Replaces the
    // MANAGE_EXTERNAL_STORAGE → Settings deep-link flow we used at
    // targetSdk=35. Fire-and-forget: dialog shows on first launch, user
    // grants once, RealFs reads of /storage/emulated/0/... start working.
    gpui_android::storage::request_once(&app);

    // Materialize Android's active DNS servers in app-private storage. Native
    // CLI launchers inherit it as fd 9; patched binaries open
    // `/proc/self/fd/9` instead of Android's missing `/etc/resolv.conf`.
    gpui_android::dns_bridge::populate_resolv_conf(&app);
    repair_installed_cli_dns();
    if let Some(prefix) = std::env::var_os("PREFIX").map(PathBuf::from)
        && let Err(err) =
            zdroid_runtime::adapters::bootstrap_install::ensure_package_manager_launchers(&prefix)
    {
        log::warn!("zed_android: package-manager launcher setup failed: {err:#}");
    }

    // Install zd-exec into <data>/files/bin/ from the APK-bundled
    // asset. Idempotent (skipped when on-disk byte length matches the
    // asset's), so it runs every boot but is essentially free past
    // the first launch. Without this, fresh installs land with no
    // zd-exec and `terminal.shell` (which the chroot adapter sets to
    // `<data>/files/bin/zd-exec`) fails with ENOENT. Phase 4 of the
    // Termux-divestment refactor moved this off `$PREFIX/bin/`.
    if let Err(err) = gpui_android::zd_exec_install::ensure_installed(&app, &data_path) {
        log::warn!(
            "zed_android: zd-exec install failed: {err:#}; \
             chroot adapter's integrated terminal will fail to spawn"
        );
    }

    // Populate <data>/files/zd-runtime/ with symlinks for every binary
    // the active runtime adapter advertises. Each symlink points at
    // zd-exec; kernel PATH lookup intercepts Zed's
    // `Command::new("java")` and routes through the bridge to wherever
    // the binary actually lives. The adapter inspects its OWN
    // filesystem (chroot walks the rootfs's /usr/bin etc.; bootstrap
    // walks $PREFIX/bin) — no hardcoded list anywhere. apt-installing
    // a new tool inside the chroot makes it show up after the next
    // launch; switching adapters rewrites the set to match the new
    // env (stale entries get swept). Pre-Phase-4 this lived at
    // `$PREFIX/zd-runtime/`.
    if let Some(provider) = build_active_provider(&data_path) {
        let binaries = provider.list_binaries();
        log::info!(
            "zed_android: zd-runtime: {} binaries from {:?} adapter",
            binaries.len(),
            provider.id(),
        );
        if let Err(err) =
            gpui_android::zd_exec_install::ensure_runtime_symlinks(&data_path, &binaries)
        {
            log::warn!(
                "zed_android: zd-runtime symlinks failed: {err:#}; \
                 LSPs that resolve binaries via PATH will fail to spawn"
            );
        }
    } else {
        log::info!(
            "zed_android: zd-runtime: no active adapter; PATH interception \
             skipped (runtime picker will set one up after first selection)"
        );
    }

    // Wire askpass to the standalone helper. Must happen BEFORE any
    // AskPassSession is created (Open Remote, git auth prompts, etc.)
    // — the askpass crate's ASKPASS_PROGRAM OnceLock initializes on
    // first read with current_exe() (= /system/bin/app_process64 on
    // Android) and subsequent set_program calls are silently ignored.
    let askpass_path = match gpui_android::askpass_install::ensure_installed(&app, &data_path) {
        Ok(path) => path,
        Err(err) => {
            log::warn!(
                "zed_android: askpass helper install failed: {err:#}; \
                 SSH password / passphrase prompts will fall back to \
                 current_exe() (= app_process64) and SIGABRT on Android"
            );
            // Construct the expected path anyway so set_program isn't
            // skipped — if the binary materializes later (next boot
            // after the install issue is resolved) it'll be picked up.
            data_path.join("zed-askpass-helper")
        }
    };
    if askpass_path.is_file() {
        match askpass::set_program(askpass_path.clone()) {
            Ok(()) => log::info!(
                "zed_android: askpass program set to {}",
                askpass_path.display()
            ),
            Err(_) => log::warn!(
                "zed_android: askpass::set_program rejected (OnceLock \
                 already initialized — set_program must run BEFORE first \
                 AskPassSession)"
            ),
        }
    } else {
        log::warn!(
            "zed_android: askpass helper missing at {}; SSH password / \
             passphrase prompts will fall through to current_exe() and \
             abort under SELinux",
            askpass_path.display()
        );
    }
    if let Err(err) = gpui_android::github_credentials::start(&app, &data_path) {
        log::warn!("zed_android: terminal GitHub credential bridge failed: {err:#}");
    }

    let dns_resolver = AndroidDnsResolver::new(&app);
    gpui_android::run(app, assets::Assets, move |cx: &mut App| {
        if let Err(err) = boot(cx, &data_path, dns_resolver) {
            error!("zed_android: boot failed: {err:#}");
        }
    });
}

/// Bind the default keymap plus the vim keymap when vim/helix mode is
/// enabled. Mirrors `crates/zed/src/zed.rs::load_default_keymap` /
/// `reload_keymaps`: clears all bindings and re-binds from scratch so
/// disabling vim mode actually removes the vim keybindings instead of
/// leaving them stacked on top of the defaults.
///
/// Called once during boot AFTER all `_init(cx)` calls have registered
/// their actions (otherwise `load_asset_allow_partial_failure` silently
/// drops bindings for unknown actions). Re-invoked on every
/// `SettingsStore` change so toggling `vim_mode` from the command
/// palette or settings.json takes effect immediately.
/// Run the in-app updater asynchronously. Used by both the
/// `auto_update::Check` action handler (menu-triggered, foreground)
/// and the startup auto-check (`silent_when_up_to_date=true` so a
/// successful check doesn't pop a "you're on the latest" prompt at
/// every boot). The flow is:
///   1. Background-fetch the latest GitHub release tag + compare
///      against the running app version.
///   2. If newer is available: prompt the user on the foreground
///      thread → if they accept, background-download the APK → hand
///      to Android's installer via `MainActivity.launchPackageInstaller`.
///   3. If up to date and not silent: prompt informationally.
///   4. On any error: log + show a prompt with the error text (or
///      silently swallow if `silent_when_up_to_date` is true).
fn run_update_check(
    silent_when_up_to_date: bool,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<Workspace>,
) {
    let Some(guard) = gpui_android::updater::UpdateGuard::try_acquire() else {
        info!("zed_android: update already in progress, skipping new request");
        return;
    };
    cx.spawn_in(window, async move |_workspace, cx| {
        // Hold the guard for the duration of the task so a second
        // dispatch (e.g. menu click during the download) is rejected
        // by `try_acquire`.
        let _guard = guard;
        let check = cx
            .background_executor()
            .spawn(async { gpui_android::updater::check_for_update() })
            .await;
        let result = match check {
            Ok(r) => r,
            Err(err) => {
                error!("zed_android: update check failed: {err:#}");
                if !silent_when_up_to_date {
                    let _ = cx.update(|window, cx| {
                        window.prompt(
                            gpui::PromptLevel::Warning,
                            "Update check failed",
                            Some(&format!("{err:#}")),
                            &["OK"],
                            cx,
                        )
                    });
                }
                return;
            }
        };
        match result {
            gpui_android::updater::UpdateCheck::UpToDate { current, latest } => {
                info!("zed_android: update check up to date (current={current} latest={latest})");
                if !silent_when_up_to_date {
                    let _ = cx.update(|window, cx| {
                        window.prompt(
                            gpui::PromptLevel::Info,
                            "Zdroid is up to date",
                            Some(&format!(
                                "You're running v{current}, which is the latest release."
                            )),
                            &["OK"],
                            cx,
                        )
                    });
                }
            }
            gpui_android::updater::UpdateCheck::Available {
                current,
                latest,
                download_urls,
            } => {
                let answer_rx = cx.update(|window, cx| {
                    window.prompt(
                        gpui::PromptLevel::Info,
                        &format!("Zdroid v{latest} is available"),
                        Some(&format!(
                            "You're on v{current}. Download and install the update now?"
                        )),
                        &["Install", "Later"],
                        cx,
                    )
                });
                let answer = match answer_rx {
                    Ok(rx) => rx.await.ok(),
                    Err(_) => None,
                };
                if answer != Some(0) {
                    info!("zed_android: update v{latest} declined by user");
                    return;
                }
                let download_tag = latest.clone();
                let download = cx
                    .background_executor()
                    .spawn(async move {
                        gpui_android::updater::download_apk(&download_tag, &download_urls)
                    })
                    .await;
                let apk_path = match download {
                    Ok(p) => p,
                    Err(err) => {
                        error!("zed_android: APK download failed: {err:#}");
                        let _ = cx.update(|window, cx| {
                            window.prompt(
                                gpui::PromptLevel::Warning,
                                "Update download failed",
                                Some(&format!("{err:#}")),
                                &["OK"],
                                cx,
                            )
                        });
                        return;
                    }
                };
                if let Err(err) = gpui_android::updater::launch_installer(&apk_path) {
                    error!("zed_android: launch_installer failed: {err:#}");
                    let _ = cx.update(|window, cx| {
                        window.prompt(
                            gpui::PromptLevel::Warning,
                            "Couldn't launch the installer",
                            Some(&format!("{err:#}")),
                            &["OK"],
                            cx,
                        )
                    });
                }
            }
        }
    })
    .detach();
}

fn reload_zdroid_keymaps(cx: &mut App) {
    cx.clear_key_bindings();
    match settings::KeymapFile::load_asset_allow_partial_failure(settings::DEFAULT_KEYMAP_PATH, cx)
    {
        Ok(bindings) => {
            info!(
                "zed_android: loaded {} key bindings from default keymap",
                bindings.len(),
            );
            cx.bind_keys(bindings);
        }
        Err(err) => error!("zed_android: default keymap load failed: {err:#}"),
    }
    let vim_enabled = vim_mode_setting::VimModeSetting::is_enabled(cx)
        || vim_mode_setting::HelixModeSetting::is_enabled(cx);
    if vim_enabled {
        match settings::KeymapFile::load_asset_allow_partial_failure(settings::VIM_KEYMAP_PATH, cx)
        {
            Ok(bindings) => {
                info!(
                    "zed_android: loaded {} vim/helix key bindings",
                    bindings.len(),
                );
                cx.bind_keys(bindings);
            }
            Err(err) => error!("zed_android: vim keymap load failed: {err:#}"),
        }
    }
}

fn boot(cx: &mut App, data_path: &std::path::Path, dns_resolver: AndroidDnsResolver) -> Result<()> {
    // android_main can run multiple times for one process when the activity
    // is recreated; paths' OnceLocks survive across invocations. The second
    // call panics with "set_custom_data_dir called after data_dir or
    // config_dir was initialized". Guard with a static flag.
    static PATHS_INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = PATHS_INITIALIZED.get_or_init(|| {
        // Each runtime adapter (chroot / bootstrap / external Termux)
        // gets a FULLY ISOLATED Zed install: its own config, keymap,
        // themes, sqlite db, language servers, extensions, history,
        // logs — everything. Switching adapters via the runtime picker
        // is a workspace switch the way booting from a different SSD
        // is a workspace switch: nothing bleeds across.
        //
        // We do this by setting `paths::set_custom_data_dir` to the
        // active adapter's `environment_root` instead of the generic
        // app data dir. Every `paths::*` function — `config_dir`,
        // `database_dir`, `logs_dir`, `languages_dir`,
        // `extensions_dir`, the lot — derives from this single root.
        // Zed never has to know about adapters.
        //
        // First launch (no runtime.toml yet) falls back to the plain
        // app data dir so the runtime picker UI itself can render and
        // the user can pick an adapter. After they pick + reboot, this
        // branch goes through `build_active_provider` and lands them
        // in the per-adapter root.
        let root = if let Some(provider) = build_active_provider(data_path) {
            let env_root = provider.environment_root();
            log::info!(
                "zed_android: data_dir for {:?} adapter -> {}",
                provider.id(),
                env_root.display(),
            );
            // Test writability before handing the path to paths::set_custom_data_dir,
            // which panics in `create_dir_all` if the path lives in another package's
            // data dir (the ExternalTermux adapter's environment_root points at
            // /data/data/com.termux/... which we can't write to under SELinux). On
            // PermissionDenied we fall back to the bare app data dir so the editor
            // can still boot; the adapter's spawn-side dispatch (env_for_zed_process,
            // zd-exec routing) stays unaffected because that's a separate seam from
            // the editor's data_dir. Without this, picking ExternalTermux in the
            // runtime picker permanently bricks the install in a boot-time panic
            // loop with no in-app recovery path.
            let env_root_usable = match std::fs::create_dir_all(&env_root) {
                Ok(()) => true,
                Err(err) => {
                    log::warn!(
                        "zed_android: {:?} adapter's environment_root {} is not \
                         writable ({err:#}); falling back to bare app data dir for \
                         editor data. Spawn-side dispatch is unaffected.",
                        provider.id(),
                        env_root.display(),
                    );
                    false
                }
            };
            // Register the env_root with util::command so absolute-path
            // spawns under it get rewritten to route through `zd-exec`.
            // Gated on `needs_command_bridge()` so adapters whose
            // binaries run natively on the host (bootstrap's bionic-
            // flavored Termux prefix, external Termux's Intent
            // dispatch) skip the rewrite. Without that gate, every
            // absolute-path spawn from a bootstrap user (PATH-resolved
            // rust-analyzer at $PREFIX/bin/rust-analyzer, downloaded
            // LSPs under <data>/languages/, etc.) would rewrite to
            // `zd-exec <abs>` and fail with ENOENT — bootstrap mode
            // doesn't put zd-exec on PATH.
            if provider.needs_command_bridge() {
                util::command::register_environment_root(env_root.clone());
            }
            if env_root_usable {
                env_root.to_string_lossy().into_owned()
            } else {
                data_path.to_string_lossy().into_owned()
            }
        } else {
            log::info!(
                "zed_android: no runtime.toml; using bare app data_dir {} \
                 (the runtime picker will set up per-adapter roots after first selection)",
                data_path.display(),
            );
            data_path.to_string_lossy().into_owned()
        };
        paths::set_custom_data_dir(&root);
    });
    info!("zed_android: paths data_dir set");

    // Production zed creates these in `init_paths()` at boot. Without
    // them, anything using `update_settings_file` (Onboarding theme /
    // keymap toggles, settings_ui writes, workspace serialization)
    // tries to atomic_write into a directory that doesn't exist, fails,
    // and short-circuits before even applying the in-memory mutation —
    // so toggles look like no-ops.
    let termux_home = data_path.join("home");
    let projects_dir = termux_home.join("projects");
    for path in [
        paths::config_dir(),
        paths::database_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
        paths::languages_dir(),
        &termux_home,
        // Default landing dir for the project picker. Lives on app-private
        // storage where execve and dlopen work natively, so cargo / go /
        // make / native-npm / native-pip all just run. SAF-picked projects
        // get imported here; nothing executable ever runs from /sdcard.
        &projects_dir,
    ] {
        if let Err(err) = std::fs::create_dir_all(path) {
            error!("zed_android: create_dir_all({}): {err:#}", path.display());
        }
    }
    // Termux-style ~/storage/* curated symlinks into shared storage. Lets
    // users browse / open / save individual files from /sdcard via
    // ~/storage/{shared,downloads,dcim,...} without ever treating those
    // paths as a workspace root (which would hit the FUSE noexec wall on
    // any compile-and-run flow). Idempotent on re-launch.
    gpui_android::storage::setup_user_symlinks(&termux_home);

    // Pre-trust every repo. Files under /storage/emulated/0 are owned by
    // media_rw (UID 1023) but we run as the app's per-app UID, so libgit2's
    // dubious-ownership check fires for every repo a user opens. The
    // `Trust Directory` button in git_panel writes `safe.directory = <path>`
    // to ~/.gitconfig per-repo; this skips that prompt globally by setting
    // `safe.directory = *`. Idempotent: only writes if the file doesn't
    // already exist (we never want to clobber a user's git config).
    let gitconfig = data_path.join(".gitconfig");
    if !gitconfig.exists() {
        let body = "[safe]\n\tdirectory = *\n";
        match std::fs::write(&gitconfig, body) {
            Ok(()) => info!("zed_android: wrote default ~/.gitconfig with safe.directory = *"),
            Err(err) => error!(
                "zed_android: failed to write ~/.gitconfig at {}: {err:#}",
                gitconfig.display()
            ),
        }
    }

    // Android has a real Keystore-backed gpui credential implementation.
    // Debug Zed builds otherwise select DevelopmentCredentialsProvider,
    // which writes API keys and OAuth refresh tokens to a plain JSON file.
    unsafe {
        std::env::set_var("ZED_DEVELOPMENT_USE_KEYCHAIN", "1");
    }
    release_channel::init(semver::Version::new(0, 1, 0), cx);
    info!("zed_android: release_channel init");

    info!("zed_android: http client init");
    let http =
        ReqwestClient::user_agent_with_dns_resolver("zed_android/0.1", Arc::new(dns_resolver))?;
    cx.set_http_client(Arc::new(http));
    info!("zed_android: http client set");

    info!("zed_android: settings + theme + editor init");
    settings::init(cx);
    // `gpui_tokio::init` registers the global Tokio runtime that wasmtime's
    // async support taps into for extension epoch interruption + WASI
    // async tasks. Production calls this at zed/src/main.rs:462. Without
    // it, extension_host's first WASM extension load panics:
    //   `RustPanic: no state of type gpui_tokio::GlobalTokio exists`
    // (caught it once on Android, then added this).
    gpui_tokio::init(cx);
    // `extension::init` creates the global ExtensionHostProxy that all
    // extension contribution registries (theme, language, debug-adapter)
    // hang off of. Has to run BEFORE any of those `*_extension::init`
    // calls. Cheap — just installs a default proxy global; the real
    // store gets created later by `extension_host::init` once fs/client
    // /node_runtime are available.
    extension::init(cx);
    theme_settings::init(theme::LoadThemes::All(Box::new(assets::Assets)), cx);
    editor::init(cx);

    // Seed the `onboarding::runtime_global::ActiveRuntime` global with
    // the user's runtime.toml selection — NOT `active_provider`'s
    // result. `active_provider` falls back to a default Bootstrap
    // adapter when no toml exists so env init has something to ask;
    // that fallback is NOT a user selection and the onboarding label
    // must not claim "Current: Bootstrap" for a user who never picked
    // anything. Read the toml directly: `None` flows to the label as
    // "Not configured yet" and disappears the instant the picker
    // writes a selection via `cx.set_global` (see runtime_picker.rs).
    let current_from_toml = RuntimeFile::load(&data_path.join("usr/etc/zd-runtime.toml"))
        .ok()
        .flatten()
        .map(|f| f.runtime.kind);
    cx.set_global(onboarding::runtime_global::ActiveRuntime {
        current: current_from_toml,
    });

    let app_db = AppDatabase::new();
    cx.set_global(app_db);
    info!("zed_android: AppDatabase opened + set as global");

    let session_id = uuid::Uuid::new_v4().to_string();
    let session = gpui::block_on(Session::new(session_id, KeyValueStore::global(cx)));
    info!("zed_android: Session opened");

    let client = Client::production(cx);
    info!(
        "zed_android: Client::production constructed (id={})",
        client.id()
    );

    // Initialize auto_update so the remote_server CDN-fetch path can
    // resolve the right binary URL via `GlobalAutoUpdate`. The
    // remote/transport.rs flow at `auto_update.rs:516` reads this
    // global to know which `zed-remote-server` asset to fetch from
    // GitHub releases; without it, every Open Remote attempt hard-bails
    // with "auto-update not initialized" before any download starts.
    //
    // We deliberately set `ZED_UPDATE_EXPLANATION` ahead of `init` so
    // the polling subscription (auto_update.rs:248-262) is suppressed —
    // we DO NOT want Zed periodically self-updating the APK at runtime
    // (Android distribution = user reinstalls the APK, not in-app
    // update). The explanation string is shown if the user manually
    // hits "Check for updates", redirecting them to reinstall.
    unsafe {
        std::env::set_var(
            "ZED_UPDATE_EXPLANATION",
            "Updates ship via the APK; reinstall to upgrade.",
        );
    }

    // auto_update::Check action interception. Same gpui dispatch trap
    // as the OpenSettings deep-link: `auto_update::init` (just below)
    // calls cx.observe_new to register the upstream Check handler at
    // workspace_actions[0]. Upstream's handler calls `auto_update::check`
    // which, with ZED_UPDATE_EXPLANATION set, just shows a "Zed was
    // installed via a package manager" prompt instead of our actual
    // in-app updater. Bubble-phase action dispatch auto-stops after
    // the first listener that does not call cx.propagate, so any
    // Check handler we register later (e.g. inside the big workspace
    // observe_new further down) never runs.
    //
    // Fix: register our handler BEFORE auto_update::init so it sits
    // at workspace_actions[0] and runs first. The handler routes
    // straight to gpui_android::updater (GitHub Releases fetch + APK
    // download + system installer handoff) and intentionally does
    // not propagate, so upstream's package-manager prompt is bypassed.
    // Direct calls to `auto_update::check()` (e.g. from the title bar
    // collab "Please update Zed" button) still hit the prompt; that
    // is acceptable because those callsites bypass the action system
    // and our interception cannot reach them without forking the
    // function.
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(
            |_workspace: &mut Workspace, _: &auto_update::Check, window, cx| {
                run_update_check(/*silent_when_up_to_date=*/ false, window, cx);
            },
        );
    })
    .detach();

    auto_update::init(client.clone(), cx);
    info!("zed_android: auto_update::init complete (polling suppressed)");

    let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
    info!("zed_android: UserStore + WorkspaceStore constructed");

    let fs: Arc<dyn Fs> = Arc::new(RealFs::new(None, cx.background_executor().clone()));
    <dyn Fs>::set_global(fs.clone(), cx);
    // Real NodeRuntime, mirroring crates/zed/src/main.rs:496-518. The
    // earlier port stage stubbed this out as `NodeRuntime::unavailable()`
    // back when Termux's Node didn't run on bionic — pre L3 npm intercept
    // architecture (project_l3_npm_intercept memory). The intercept layer
    // (patched node platform string, npm wrapper, launcher-gen RUNPATH
    // fixup, libtermux-exec LD_PRELOAD) plus the bundled musl loader make
    // a Termux-installed Node usable. Wire NodeRuntime against settings
    // so PATH-resolved Node (`pkg install nodejs` → $PREFIX/bin/node)
    // works, and Zed's managed-node fallback download path is available
    // for users without termux Node. Without this, npm-based LSPs
    // (TypeScript / JavaScript / Pyright / etc.) fail at the install step
    // with `'node' settings do not allow any way to use Node.js`.
    let (mut node_options_tx, node_options_rx) =
        watch::channel::<Option<node_runtime::NodeBinaryOptions>>(None);
    cx.observe_global::<SettingsStore>(move |cx| {
        let settings = &project::project_settings::ProjectSettings::get_global(cx).node;
        let options = node_runtime::NodeBinaryOptions {
            allow_path_lookup: !settings.ignore_system_version,
            allow_binary_download: true,
            use_paths: settings.path.as_ref().map(|node_path| {
                let node_path = std::path::PathBuf::from(shellexpand::tilde(node_path).as_ref());
                let npm_path = settings
                    .npm_path
                    .as_ref()
                    .map(|p| std::path::PathBuf::from(shellexpand::tilde(&p).as_ref()));
                (
                    node_path.clone(),
                    npm_path.unwrap_or_else(|| {
                        node_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_default()
                            .join("npm")
                    }),
                )
            }),
        };
        node_options_tx.send(Some(options)).log_err();
    })
    .detach();
    let node_runtime = NodeRuntime::new(client.http_client(), None, node_options_rx);
    info!("zed_android: RealFs + NodeRuntime constructed (PATH lookup + managed download)");

    // Mirror production zed::watch_settings_files. Without this, edits to
    // ~/.config/zed/settings.json on disk never propagate into the running
    // app — themes, keybindings, terminal.shell etc. only honoured on
    // restart. Production also wires migration notifications; we skip those
    // because we don't ship the upgrade path UI yet.
    settings::SettingsStore::update_global(cx, |store, cx| {
        store.watch_settings_files(fs.clone(), cx, |settings_file, result, _cx| {
            if let settings::ParseStatus::Failed { error } = &result.parse_status {
                log::error!("zed_android: settings parse failed ({settings_file:?}): {error}");
            }
        });
    });
    info!("zed_android: settings file watcher attached");

    // Mirror `android_input.*` settings into gpui_android's runtime
    // atomics directly via observe_global, NOT via per-paint
    // `window.set_*` writes from `workspace::pane`. The pane path
    // only fires when a Pane renders — during onboarding (before a
    // project is open) there is no Pane, so the atomics stay at
    // their `true`/`false` defaults regardless of what the user
    // toggles in Settings. Symptom: opening Settings during
    // onboarding, switching `on_screen_keyboard` off, then focusing
    // a text input still pops the soft IME because the gate atomic
    // is stale. Observer fires once immediately + on every
    // SettingsStore change, so the gate is always fresh.
    let push_android_input_atomics = |cx: &gpui::App| {
        let android_input = workspace::AndroidInputSettings::get_global(cx);
        gpui_android::set_on_screen_keyboard_enabled(android_input.on_screen_keyboard);
        let trackpad_active = android_input.trackpad_mode && android_input.trackpad_mode_active;
        gpui_android::set_trackpad_mode_enabled(trackpad_active);
        gpui_android::set_programming_extras_row_enabled(android_input.programming_extras_row);
        gpui_android::set_invert_scroll(android_input.invert_scroll);
    };
    push_android_input_atomics(cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        push_android_input_atomics(cx);
    })
    .detach();
    info!("zed_android: android_input atomics observer registered");

    let apply_background_execution = |cx: &gpui::App| {
        let enabled = workspace::WorkspaceSettings::get_global(cx).background_execution;
        gpui_android::storage::set_background_execution_enabled(enabled);
    };
    apply_background_execution(cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        apply_background_execution(cx);
    })
    .detach();
    info!("zed_android: background execution observer registered");

    // Drive the vim-mode soft-keyboard routing gate. In a vim command
    // mode (Normal / Visual / operator-pending / Helix) soft-keyboard
    // text has to arrive as key *events* so vim's keymap reads `j`/`d`/
    // `w` as motions and operators; only Insert and Replace insert the
    // literal characters. Replace is the trap — it feels like a command
    // mode but is text entry, so it routes as text like Insert.
    //
    // Mirrors `vim::ModeIndicator`: a holder entity keeps a single
    // subscription to the *focused* vim, swapped out on each `Focused`
    // event, so an unfocused split-pane editor flipping its own mode
    // can't clobber the gate. The holder is owned by the detached
    // `observe_new` closure, so it lives for the whole process. Desktop
    // never reaches this — it has no soft keyboard — which is why the
    // mode read sits behind the tiny `Vim::mode()` accessor.
    struct VimImeRouter {
        focused_vim: Option<gpui::Subscription>,
    }
    fn push_vim_route(mode: vim::Mode) {
        let route_as_keys = !matches!(mode, vim::Mode::Insert | vim::Mode::Replace);
        gpui_android::set_ime_route_as_keys(route_as_keys);
    }
    let vim_ime_router = cx.new(|_| VimImeRouter { focused_vim: None });
    cx.observe_new::<vim::Vim>(move |_, _window, cx| {
        let vim = cx.entity();
        vim_ime_router.update(cx, |_, cx| {
            cx.subscribe(
                &vim,
                |router: &mut VimImeRouter, vim, event, cx| match event {
                    vim::VimEvent::Focused => {
                        push_vim_route(vim.read(cx).mode());
                        router.focused_vim = Some(
                            cx.observe(&vim, |_, vim, cx| push_vim_route(vim.read(cx).mode())),
                        );
                    }
                },
            )
            .detach();
        });
    })
    .detach();
    info!("zed_android: vim-mode IME routing observer registered");

    cx.text_system().add_fonts(
        BUNDLED_FONTS
            .iter()
            .map(|bytes| Cow::Borrowed(*bytes))
            .collect(),
    )?;

    let mut language_registry = language::LanguageRegistry::new(cx.background_executor().clone());
    language_registry.set_language_server_download_dir(paths::languages_dir().clone());
    let language_registry = Arc::new(language_registry);

    languages::init(
        language_registry.clone(),
        fs.clone(),
        node_runtime.clone(),
        cx,
    );
    info!("zed_android: languages::init complete (load-grammars feature on)");

    // language_extension::init registers grammar/language/language-server
    // proxies on the global ExtensionHostProxy so extension-installed
    // languages and LSPs route through Zed's normal language registry.
    // Must come AFTER `languages::init` (the registry has to exist) and
    // AFTER `extension::init` (the proxy has to exist).
    //
    // Production passes `LspAccess::ViaWorkspaces(...)` so extension-
    // installed LSPs get auto-registered against every active workspace.
    // We use `Noop` for now — extensions can still contribute languages,
    // grammars, and themes; LSP-from-extension will require wiring
    // `ViaWorkspaces` against the multi-workspace scope, which is its
    // own follow-up because Android's `MultiWorkspace` differs in shape
    // from desktop.
    {
        let extension_host_proxy = extension::ExtensionHostProxy::global(cx);
        language_extension::init(
            language_extension::LspAccess::Noop,
            extension_host_proxy,
            language_registry.clone(),
        );
    }

    let registry = theme::ThemeRegistry::global(cx);
    info!(
        "zed_android: theme registry has {} themes loaded",
        registry.list().len()
    );

    let app_session = cx.new(|cx| AppSession::new(session, cx));
    let app_state = Arc::new(AppState {
        languages: language_registry,
        client: client.clone(),
        user_store: user_store.clone(),
        workspace_store,
        fs: fs.clone(),
        build_window_options: minimal_window_options,
        node_runtime: node_runtime.clone(),
        session: app_session,
    });
    AppState::set_global(app_state.clone(), cx);
    info!("zed_android: AppState assembled + set as global");

    // Mirror production zed/src/main.rs:785 — without this every language's
    // tree-sitter captures parse but render with no syntax styling. Re-apply
    // on theme changes so theme toggles actually recolour text. Bracket
    // matching, indents, and outline don't depend on this and would have
    // worked already; syntax colors and rainbow brackets do.
    app_state
        .languages
        .set_theme(theme::GlobalTheme::theme(cx).clone());
    cx.observe_global::<theme::GlobalTheme>({
        let languages = app_state.languages.clone();
        move |cx| {
            languages.set_theme(theme::GlobalTheme::theme(cx).clone());
        }
    })
    .detach();

    // Adapter-aware `terminal.shell`. The picker writes runtime.toml;
    // we re-derive the spawn target every launch by asking the active
    // adapter via `RuntimeProvider::terminal_shell` so flipping the
    // adapter actually changes where the integrated terminal lands.
    //
    // Why overwrite even when the user already has a shell set: the
    // picker IS the user's terminal-target choice in this app —
    // picking chroot means "I want the terminal in the chroot's bash".
    // Honoring a stale settings.shell from a prior adapter would
    // silently contradict the picker. A user who wants something
    // custom inside the chroot configures it inside the chroot (their
    // `$SHELL`, `chsh`, etc.), not at the alacritty entry point.
    //
    // alacritty's `pw_shell` lookup on Android returns /system/bin/sh
    // (the parody) so this explicit Shell::Program is what makes any
    // useful terminal work at all.
    let provider = active_provider(data_path);
    let shell_path = provider
        .terminal_shell(data_path)
        .unwrap_or_else(|| data_path.join("usr/bin/bash"));
    if shell_path.is_file() {
        let shell_str = shell_path.to_string_lossy().to_string();
        let fs_for_settings = app_state.fs.clone();
        cx.global::<settings::SettingsStore>().update_settings_file(
            fs_for_settings,
            move |content, _cx| {
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                let new_shell = settings::Shell::Program(shell_str.clone());
                let prev = terminal.project.shell.clone();
                terminal.project.shell = Some(new_shell);
                log::info!(
                    "zed_android: terminal.shell -> {} (was {:?}, adapter-derived)",
                    shell_str,
                    prev,
                );
            },
        );
    } else {
        log::warn!(
            "zed_android: adapter-derived shell {} missing on disk; \
             leaving terminal.shell as-is",
            shell_path.display()
        );
    }

    Client::set_global(client.clone(), cx);
    client::init(&client, cx);
    // Register the TrustedWorktrees global. git_store reads it via
    // `try_get_global` when a repo is added; without it, `is_trusted`
    // defaults to false, which routes every repo into the
    // dubious-ownership UI in git_panel.rs:4862.
    //
    // Mirror production zed/src/main.rs:450-457 — fetch the persisted
    // trust grants from `WorkspaceDb::fetch_trusted_worktrees()` so a
    // user's prior "Trust" choice survives across launches and
    // reinstalls. Previously we passed `HashMap::default()` here, which
    // wiped the in-memory trust map every boot even though the SQLite
    // db was preserving it correctly — every relaunch re-prompted the
    // restricted-mode trust dialog. Fall back to empty on fetch failure
    // (typically only happens if the db schema upgrade is mid-flight).
    let db_trusted_paths = match workspace::WorkspaceDb::global(cx).fetch_trusted_worktrees() {
        Ok(paths) => paths,
        Err(err) => {
            error!(
                "zed_android: fetch_trusted_worktrees failed at boot: \
                     {err:#} — starting with empty trust map; user will \
                     be re-prompted for any previously trusted projects"
            );
            std::collections::HashMap::default()
        }
    };
    project::trusted_worktrees::init(db_trusted_paths, cx);
    Project::init(&client, cx);

    // Extensions (Phase L3 — previously deferred). Mirrors production
    // zed/src/main.rs:623, 631–637, 741. The store creates the
    // global ExtensionStore, watches `paths::extensions_dir()` for new
    // installations, and asynchronously fetches the registry from
    // Zed's API. Theme/debug-adapter proxies register against the
    // already-installed `ExtensionHostProxy` from `extension::init`
    // earlier in boot. extensions_ui registers the workspace observer
    // that handles the `zed_actions::Extensions` action — taps from
    // the title-bar settings menu open the browse/install pane.
    //
    // Wasmtime engine init (`crates/extension_host/src/wasm_host.rs:564`)
    // currently uses Cranelift JIT. Whether that survives Android's
    // `untrusted_app_27` SELinux W^X policy is open — modern Android
    // (API 30+) typically allows anonymous executable mappings for
    // app processes, so it MAY just work; if not, the engine init
    // will panic at first extension load and we switch wasmtime's
    // strategy to Pulley interpreter (one Cargo.toml feature flip +
    // a `Config::strategy(Strategy::Pulley)` line). See
    // `deferred-render-pipeline-perf.md` philosophy: read the actual
    // failure first, don't preemptively configure for a problem that
    // may not exist.
    {
        let extension_host_proxy = extension::ExtensionHostProxy::global(cx);
        extension_host::init(
            extension_host_proxy.clone(),
            fs.clone(),
            client.clone(),
            node_runtime.clone(),
            cx,
        );
        debug_adapter_extension::init(extension_host_proxy.clone(), cx);
        theme_extension::init(
            extension_host_proxy,
            theme::ThemeRegistry::global(cx),
            cx.background_executor().clone(),
        );
    }

    diagnostics::init(cx);
    workspace::init(app_state.clone(), cx);
    command_palette::init(cx);
    search::init(cx);
    // Mirror production zed/src/main.rs:710. setup_search_bar is fired
    // from `terminal_panel.rs::TerminalPanel::new` ONLY — i.e. when the
    // terminal panel adds its own internal buffer search. It is NOT
    // called for editor panes; production wires those via initialize_pane
    // in zed/src/zed.rs:1234, which we mirror further down inside the
    // workspace observe_new (so editor-pane toolbars get BufferSearchBar
    // + ProjectSearchBar there).
    cx.set_global(workspace::PaneSearchBarCallbacks {
        setup_search_bar: |languages, toolbar, window, cx| {
            let search_bar = cx.new(|cx| search::BufferSearchBar::new(languages, window, cx));
            toolbar.update(cx, |toolbar, cx| {
                toolbar.add_item(search_bar, window, cx);
            });
        },
        wrap_div_with_search_actions: search::buffer_search::register_pane_search_actions,
    });
    vim::init(cx);
    // Modal pickers / panels — mirror production zed/src/main.rs init order.
    // Each registers its own actions + a SettingsStore observer if needed.
    // Skipped from production (non-portable on Android): audio/call/livekit
    // (collab), agent_ui/copilot/language_models (AI), debugger_ui/repl
    // (DAP/Jupyter), auto_update (we self-distribute), telemetry/crashes,
    // extension_host (deferred to L3).
    go_to_line::init(cx);
    file_finder::init(cx);
    tab_switcher::init(cx);
    outline::init(cx);
    project_symbols::init(cx);
    project_panel::init(cx);
    outline_panel::init(cx);
    tasks_ui::init(cx);
    snippet_provider::init(cx);
    snippets_ui::init(cx);
    image_viewer::init(cx);
    csv_preview::init(cx);
    svg_preview::init(cx);
    markdown_preview::init(cx);
    encoding_selector::init(cx);
    language_selector::init(cx);
    line_ending_selector::init(cx);
    toolchain_selector::init(cx);
    theme_selector::init(cx);
    settings_profile_selector::init(cx);
    language_tools::init(cx);
    feedback::init(cx);
    // language_model::init only registers the GlobalLanguageModelRegistry —
    // no actual AI runtime spins up. git_panel.rs reads from this registry
    // for the optional commit-message generation; without it, the panel
    // panics at first paint with "no state of type
    // language_model::registry::GlobalLanguageModelRegistry exists".
    language_model::init(cx);
    client::RefreshLlmTokenListener::register(client.clone(), user_store.clone(), cx);
    language_models::init(user_store.clone(), client.clone(), cx);
    acp_tools::init(cx);
    web_search::init(cx);
    web_search_providers::init(client.clone(), user_store.clone(), cx);
    let prompt_builder = PromptBuilder::load(app_state.fs.clone(), false, cx);
    ensure_cli_subscription_agents(app_state.fs.clone(), cx);
    project::AgentRegistryStore::init_global(
        cx,
        app_state.fs.clone(),
        app_state.client.http_client(),
    );
    agent_ui::init(
        app_state.fs.clone(),
        prompt_builder,
        app_state.languages.clone(),
        false,
        false,
        cx,
    );
    agent::init_user_agents_md(app_state.fs.clone(), cx, |state, _cx| {
        if let Some(error) = state.error() {
            log::warn!("zed_android: failed to load AGENTS.md: {error}");
        }
    });
    info!("zed_android: agent_ui + language model providers initialized");
    git_ui::init(cx);
    // Mirror production zed/src/main.rs:733 — register the git graph
    // (commit history) view's serializable item, action handlers
    // (git::FileHistory, git_panel::Open, OpenAtCommit), and database
    // domain. Action-driven: shows up as a workspace pane item when the
    // user triggers it (e.g. via git panel "View History"), not pre-
    // loaded as a panel like ProjectPanel/GitPanel. Uses
    // project.git_store() so remote-SSH projects work transparently.
    git_graph::init(cx);
    // Production zed/src/main.rs:741. Registers the workspace observer
    // that handles `zed_actions::Extensions::default()` — opens the
    // browse/install/manage pane (an `ExtensionsPage` workspace item).
    // The settings-menu chevron in the Android title bar already
    // dispatches `zed_actions::Extensions` (see title_bar.rs); without
    // this init the action goes nowhere.
    extensions_ui::init(cx);
    keymap_editor::init(cx);
    inspector_ui::init(app_state.clone(), cx);
    json_schema_store::init(cx);
    recent_projects::init(cx);
    which_key::init(cx);

    // OpenSettings interception for the not-yet-configured-runtime case.
    // Terminal failures (and several other "Edit Settings" affordances
    // scattered across Zed's UI) dispatch `zed_actions::OpenSettings`,
    // which normally lands the user at the settings.json editor. Without
    // a working Android runtime that panel can't solve anything: the user
    // has to find Android Runtime > Open picker first. Skip the dead-end
    // by routing OpenSettings straight to the picker when the runtime
    // toml is missing.
    //
    // This observe_new MUST be registered BEFORE `settings_ui::init`
    // because gpui's bubble-phase action dispatch auto-stops after the
    // first listener that doesn't explicitly call `cx.propagate()`. To
    // sit at position 0 on each workspace's dispatch-node listener list
    // (so our handler runs before `settings_ui`'s) we need to register
    // our observe_new first; `register_action` appends in observe-fire
    // order. Reordering this past `settings_ui::init` silently breaks
    // the deep-link.
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(
            |_workspace: &mut Workspace, _: &zed_actions::OpenSettings, window, cx| {
                if std::path::Path::new("/data/data/com.zdroid/files/usr/etc/zd-runtime.toml")
                    .exists()
                {
                    // Runtime configured: let settings_ui's handler open
                    // the normal settings editor.
                    cx.propagate();
                    return;
                }
                log::info!(
                    "zed_android: OpenSettings routed to runtime picker (no zd-runtime.toml yet)"
                );
                runtime_picker::open_runtime_picker_window(window, cx);
            },
        );
    })
    .detach();

    settings_ui::init(cx);
    workspace::init_settings_file_actions(cx);
    terminal_view::init(cx);
    // Onboarding reads `AllAgentServersSettings` from the SettingsStore;
    // `SettingsStore::get` panics if the type isn't registered, so register
    // it before any onboarding render. agent_settings doesn't expose a
    // separate init function — registering the type is the whole step.
    project::agent_server_store::AllAgentServersSettings::register(cx);
    onboarding::init(cx);
    menu_bar::register_actions(cx);
    runtime_picker::register(cx);
    info!(
        "zed_android: workspace + diagnostics + search + file_finder + outline_panel + onboarding + menu_bar init complete"
    );

    // Keymap load runs LAST: `_allow_partial_failure` skips any binding
    // whose action isn't registered yet, so loading before each crate's
    // `init` would silently drop every workspace::*, project_panel::*,
    // command_palette::*, vim::*, etc. binding. By the time we reach this
    // point everything has registered.
    reload_zdroid_keymaps(cx);
    // Mirror production's `reload_keymaps` flow on `VimModeSetting`
    // change: without this, toggling vim mode (via `ToggleVimMode`
    // action or settings.json edit) flips the setting and updates
    // the mode indicator, but no vim keybindings are bound to the
    // action dispatcher — `hjkl`/`i`/`:w` etc. fire nothing.
    cx.observe_global::<settings::SettingsStore>(|cx| {
        reload_zdroid_keymaps(cx);
    })
    .detach();

    // Mirror production's zed/src/zed.rs `initialize_panels`: every
    // newly-constructed workspace asynchronously loads each panel and
    // attaches it. Production loads project / outline / terminal / git /
    // collab / debug / agent panels in parallel; we only ship the ones
    // whose deps are already wired (project_panel, outline_panel for now).
    // The PanelButtons in the status bar render one button per
    // registered panel, so adding more panels here surfaces more
    // bottom-bar buttons for free.
    let observe_app_state = app_state.clone();
    cx.observe_new(move |workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };

        // Agent history is implemented by the shared Sidebar thread switcher.
        // The Android boot path does not run crates/zed's initialize_window,
        // so register the same Sidebar here after MultiWorkspace construction.
        // Creating it in a defer avoids Sidebar::new re-entering a currently
        // borrowed MultiWorkspace.
        if let Some(multi_workspace) = workspace
            .multi_workspace()
            .and_then(|multi_workspace| multi_workspace.upgrade())
            && multi_workspace.read(cx).sidebar().is_none()
        {
            cx.spawn_in(window, async move |workspace_handle, cx| {
                workspace_handle.update_in(cx, |_, window, cx| {
                    let sidebar =
                        cx.new(|cx| sidebar::Sidebar::new(multi_workspace.clone(), window, cx));
                    multi_workspace.update(cx, |multi_workspace, cx| {
                        multi_workspace.register_sidebar(sidebar, cx);
                    });
                    log::info!("zed_android: registered MultiWorkspace sidebar");
                })?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }

        workspace
            .register_action(agent_ui::AgentPanel::toggle_focus)
            .register_action(agent_ui::AgentPanel::focus)
            .register_action(agent_ui::AgentPanel::toggle)
            .register_action(agent_ui::InlineAssistant::inline_assist);

        // CloseProject: action wired in production at
        // `crates/zed/src/zed.rs:1123-1180`. Production binds it inside
        // `initialize_workspace` next to NewFile / NewWindow handlers.
        // We don't pull crates/zed (Android workspace example owns its
        // own boot()), so the handler has to be re-registered here on
        // every new workspace via observe_new.
        //
        // Flow, identical to production except the new-workspace init
        // closure is a no-op (we don't auto-create an empty Editor on
        // close — same shape as the returning-launch path at
        // `workspace::open_new(Default::default(), app_state, cx,
        // |_, _, _| {})` lower in this file):
        //
        //   1. prepare_to_close(ReplaceWindow) — checks dirty buffers,
        //      pops the standard "Save changes?" modal if needed.
        //   2. If the user proceeds, open_new() builds a fresh empty
        //      workspace within the same MultiWorkspace tab, reusing
        //      the requesting_window so we don't spawn an extra
        //      ExtraWindowActivity (which open_window does on Android).
        //   3. After the new workspace lands, remove the old project's
        //      group key from the MultiWorkspace registry. Without this
        //      the closed project's group survives in MultiWorkspace's
        //      internal tracking and `Open Recent` / window-cycle UX
        //      shows ghost entries.
        workspace.register_action({
            let app_state = observe_app_state.clone();
            move |workspace: &mut Workspace, _: &CloseProject, window, cx| {
                let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>()
                else {
                    return;
                };
                let app_state = app_state.clone();
                let old_group_key = workspace.project_group_key(cx);
                cx.spawn_in(window, async move |this, cx| {
                    let should_continue = this
                        .update_in(cx, |workspace, window, cx| {
                            workspace.prepare_to_close(CloseIntent::ReplaceWindow, window, cx)
                        })?
                        .await?;
                    if !should_continue {
                        return Ok::<(), anyhow::Error>(());
                    }
                    let task = cx.update(|_window, cx| {
                        open_new(
                            OpenOptions {
                                requesting_window: Some(window_handle),
                                ..Default::default()
                            },
                            app_state,
                            cx,
                            |_workspace, _window, _cx| {},
                        )
                    })?;
                    task.await?;
                    window_handle
                        .update(cx, |mw, window, cx| {
                            mw.remove_project_group(&old_group_key, window, cx)
                        })?
                        .await
                        .log_err();
                    Ok(())
                })
                .detach_and_log_err(cx);
            }
        });

        // Status bar items, mirroring production zed/src/zed.rs:537-586.
        // Skipped: edit_prediction_ui (AI), activity_indicator (collab),
        // merge_conflict_indicator (git_ui — pulls collab), image_info
        // (image-viewer specific). The rest are plumbed identically.
        let search_button = cx.new(|_| search::search_status_button::SearchButton::new());
        let diagnostic_summary =
            cx.new(|cx| diagnostics::items::DiagnosticIndicator::new(workspace, cx));
        let active_file_name = cx.new(|_| workspace::active_file_name::ActiveFileName::new());
        let active_buffer_encoding =
            cx.new(|_| encoding_selector::ActiveBufferEncoding::new(workspace));
        let active_buffer_language =
            cx.new(|_| language_selector::ActiveBufferLanguage::new(workspace));
        let active_toolchain_language =
            cx.new(|cx| toolchain_selector::ActiveToolchain::new(workspace, window, cx));
        let vim_mode_indicator = cx.new(|cx| vim::ModeIndicator::new(window, cx));
        let cursor_position =
            cx.new(|_| go_to_line::cursor_position::CursorPosition::new(workspace));
        let line_ending_indicator =
            cx.new(|_| line_ending_selector::LineEndingIndicator::default());
        let merge_conflict_indicator =
            cx.new(|cx| git_ui::MergeConflictIndicator::new(workspace, cx));

        // LspButton needs a handle for the toggle action, same as production.
        let lsp_button_menu_handle = ui::PopoverMenuHandle::<ui::ContextMenu>::default();
        let lsp_button = cx.new(|cx| {
            language_tools::lsp_button::LspButton::new(
                workspace,
                lsp_button_menu_handle.clone(),
                window,
                cx,
            )
        });
        workspace.register_action({
            let handle = lsp_button_menu_handle.clone();
            move |_, _: &language_tools::lsp_button::ToggleMenu, window, cx| {
                handle.toggle(window, cx);
            }
        });

        workspace.status_bar().update(cx, |status_bar, cx| {
            status_bar.add_left_item(search_button, window, cx);
            status_bar.add_left_item(lsp_button, window, cx);
            status_bar.add_left_item(diagnostic_summary, window, cx);
            status_bar.add_left_item(active_file_name, window, cx);
            status_bar.add_left_item(merge_conflict_indicator, window, cx);
            status_bar.add_right_item(active_buffer_encoding, window, cx);
            status_bar.add_right_item(active_buffer_language, window, cx);
            status_bar.add_right_item(active_toolchain_language, window, cx);
            status_bar.add_right_item(line_ending_indicator, window, cx);
            status_bar.add_right_item(vim_mode_indicator, window, cx);
            status_bar.add_right_item(cursor_position, window, cx);
        });

        // Per-pane toolbar items. Mirrors production
        // zed/src/zed.rs::initialize_pane (zed.rs:1234) which production
        // invokes from a workspace observer at zed.rs:444-451 for the
        // initial active pane and on every PaneAdded event.
        //
        //   - BufferSearchBar — the in-editor Ctrl-F find/replace bar.
        //     Already registered for terminal panels via the global
        //     PaneSearchBarCallbacks above; production registers a SECOND
        //     instance on each editor pane's toolbar (different toolbar,
        //     different visibility scope), and we mirror that here.
        //
        //   - ProjectSearchBar — the toolbar that holds the actual query
        //     input field, regex/case/whole-word toggles, replace UI,
        //     and match navigation for the Project Search view.
        //     ProjectSearchView's render() draws only the results body;
        //     the input field lives in this toolbar item. Without it
        //     registered on the pane's toolbar, the project-search view
        //     shows the empty-state heading but no input field, and the
        //     toolbar slot stays in a half-rendered state — visible as
        //     a flickering tab at vsync rate.
        //
        // Subscribe to PaneAdded so split panes get the same setup.
        let toolbar_languages = observe_app_state.languages.clone();
        let active_pane = workspace.active_pane().clone();
        active_pane.update(cx, |pane, cx| {
            pane.toolbar().update(cx, |toolbar, cx| {
                let buffer_search_bar = cx.new(|cx| {
                    search::BufferSearchBar::new(Some(toolbar_languages.clone()), window, cx)
                });
                toolbar.add_item(buffer_search_bar, window, cx);
                let project_search_bar =
                    cx.new(|_| search::project_search::ProjectSearchBar::new());
                toolbar.add_item(project_search_bar, window, cx);
            });
        });
        let workspace_handle = cx.entity();
        let pane_added_languages = observe_app_state.languages.clone();
        cx.subscribe_in(&workspace_handle, window, move |_, _, event, window, cx| {
            if let workspace::Event::PaneAdded(pane) = event {
                let languages = pane_added_languages.clone();
                pane.update(cx, |pane, cx| {
                    pane.toolbar().update(cx, |toolbar, cx| {
                        let buffer_search_bar =
                            cx.new(|cx| search::BufferSearchBar::new(Some(languages), window, cx));
                        toolbar.add_item(buffer_search_bar, window, cx);
                        let project_search_bar =
                            cx.new(|_| search::project_search::ProjectSearchBar::new());
                        toolbar.add_item(project_search_bar, window, cx);
                    });
                });
            }
        })
        .detach();

        // Custom always-on application menu bar. On Android there's no
        // native menu surface and the production `title_bar` crate pulls
        // in audio/livekit/auto_update/git_ui — too heavy for this stage.
        // The bar dispatches the same actions production's menus do, and
        // can be hidden via the chevron-up button or the
        // `ToggleAppMenuBar` action (Ctrl+Alt+M).
        let weak_workspace = cx.weak_entity();
        let menu_bar = cx.new(|inner_cx| menu_bar::MenuBar::new(weak_workspace.clone(), inner_cx));
        menu_bar::register(&menu_bar);
        // Title bar sits below the menu bar with the project name,
        // Restricted Mode trust badge, and settings chevron. The
        // chevron's tap toggles `menu_bar.hidden`; right-click /
        // two-finger tap opens the Settings/Keymap/Themes/Icon
        // Themes/Extensions dropdown.
        let title_bar = cx.new(|inner_cx| {
            title_bar::TitleBar::new(weak_workspace.clone(), menu_bar.downgrade(), inner_cx)
        });
        let header = cx.new(|_| header::Header::new(menu_bar, title_bar));
        workspace.set_titlebar_item(header.into(), window, cx);

        // Mirror production's `initialize_panels`: every newly-constructed
        // workspace asynchronously loads each panel and attaches it.
        // Production loads project / outline / terminal / git / collab /
        // debug / agent panels in parallel; we only ship the ones whose
        // deps are already wired. The PanelButtons in the status bar
        // render one button per registered panel, so adding more panels
        // surfaces more bottom-bar buttons for free.
        let weak = cx.weak_entity();
        cx.spawn_in(window, async move |_, cx| {
            let project_panel =
                project_panel::ProjectPanel::load(weak.clone(), cx.clone());
            let outline_panel =
                outline_panel::OutlinePanel::load(weak.clone(), cx.clone());
            let terminal_panel =
                terminal_view::terminal_panel::TerminalPanel::load(weak.clone(), cx.clone());
            let git_panel =
                git_ui::git_panel::GitPanel::load(weak.clone(), cx.clone());
            let agent_panel = agent_ui::AgentPanel::load(weak.clone(), cx.clone());
            let (project_panel, outline_panel, terminal_panel, git_panel, agent_panel) =
                futures::join!(
                    project_panel,
                    outline_panel,
                    terminal_panel,
                    git_panel,
                    agent_panel,
                );
            weak.update_in(cx, |workspace, window, cx| {
                if let Ok(panel) = project_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = outline_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = terminal_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = git_panel {
                    workspace.add_panel(panel, window, cx);
                }
                if let Ok(panel) = agent_panel {
                    workspace.add_panel(panel, window, cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    })
    .detach();

    // Quit action handler. Upstream `crates/zed/src/zed.rs` wires
    // `zed_actions::Quit` to `cx.quit()` in several places, but none of
    // those registration paths run in the Android entry — we don't pull
    // in `crates/zed`'s init code. Without this handler the Zdroid
    // menu's "Quit" item dispatches the action and nothing responds, so
    // the user just sees no effect. `cx.quit()` triggers gpui's
    // shutdown sequence which eventually reaches
    // `AndroidPlatform::quit` and exits the process.
    cx.on_action(|_: &zed_actions::Quit, cx: &mut App| {
        cx.quit();
    });

    // Check-for-Updates handler is registered per-workspace above so
    // it has Window access for prompts. The upstream
    // `auto_update::init` already ran above which set its own App-level
    // handler that defers to the Zed update server (irrelevant for
    // Android); our workspace-level handler takes precedence when the
    // menu fires the action inside a workspace context, which is the
    // only path users hit it from.

    // Production's `prompt_and_open_paths` assumes multi-window: when no
    // existing local workspace passes the `workspace_location` filter, it
    // opens a new window via `Workspace::new_local(.., None, ..)`. Single-
    // window Android rejects the second `cx.open_window`, so we replace
    // the `Open` action with the same call production makes once it's
    // already inside the right window — `MultiWorkspace::open_project`,
    // which `find_or_create_local_workspace`s and reuses the empty
    // workspace as the slot to swap. That path also dismisses any open
    // Onboarding/WelcomePage items naturally because the workspace gets
    // replaced.
    cx.on_action(|_: &workspace::Open, cx: &mut App| {
        // Capture the destination before launching Android DocumentsUI.
        // While its Activity result is transitioning back to Zdroid,
        // `active_window()` can briefly be None (or still point at a picker
        // window), which used to discard the selected directory.
        let active = cx.active_window();
        let target_multi_workspace = active
            .as_ref()
            .and_then(|window| window.downcast::<MultiWorkspace>());
        let target_workspace = active
            .as_ref()
            .and_then(|window| window.downcast::<workspace::Workspace>());
        // The active handle can downcast to the inner Workspace on the
        // welcome screen. Prefer the owning MultiWorkspace whenever one
        // exists so open_project replaces the welcome view and activates
        // the selected project instead of merely adding a hidden worktree.
        let target_multi_workspace = target_multi_workspace.or_else(|| {
            workspace_windows_for_location(&SerializedWorkspaceLocation::Local, cx)
                .into_iter()
                .next()
        });

        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |cx| {
            let picked = match paths.await {
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    error!("zed_android: Open picker failed: {err:#}");
                    let message = format!("{err:#}");
                    show_open_project_error(
                        target_multi_workspace.as_ref(),
                        target_workspace.as_ref(),
                        &message,
                        cx,
                    );
                    return;
                }
                Err(_) => return,
            };
            info!("zed_android: Open picker selected paths={picked:?}");

            // The first-launch onboarding path (show_onboarding_view →
            // workspace::open_new) opens a plain `Workspace` window, not
            // a `MultiWorkspace`. Returning-launch path is also `Workspace`
            // until something attaches the multi-workspace shell. The Open
            // action can fire on either, so try MultiWorkspace first then
            // fall through to plain Workspace; the previous code silently
            // no-op'd ("no active MultiWorkspace for Open" log + return)
            // when invoked on the welcome screen of a fresh install.
            if let Some(mw) = target_multi_workspace {
                let task = mw.update(cx, |mw, window, cx| {
                    mw.open_project(picked, workspace::OpenMode::Activate, window, cx)
                });
                match task {
                    Ok(task) => {
                        if let Err(err) = task.await {
                            error!("zed_android: open_project failed: {err:#}");
                            let message = format!("{err:#}");
                            show_open_project_error(Some(&mw), None, &message, cx);
                        } else {
                            info!("zed_android: open_project completed");
                        }
                    }
                    Err(err) => {
                        error!("zed_android: open_project update failed: {err:#}");
                        let message = format!("{err:#}");
                        show_open_project_error(Some(&mw), None, &message, cx);
                    }
                }
            } else if let Some(ws) = target_workspace {
                // Fall-through path: add the picked folder as a worktree
                // of the current workspace. The welcome page tab survives
                // alongside (user can close it). Not as polished as the
                // MultiWorkspace `find_or_create + replace` flow, but it
                // unblocks Open Project from the welcome screen on every
                // boot path.
                let task = ws.update(cx, |ws, window, cx| {
                    ws.open_paths(picked, workspace::OpenOptions::default(), None, window, cx)
                });
                match task {
                    Ok(task) => {
                        let opened_items = task.await;
                        info!(
                            "zed_android: workspace open_paths completed items={}",
                            opened_items.len()
                        );
                    }
                    Err(err) => {
                        error!("zed_android: workspace open_paths update failed: {err:#}");
                        let message = format!("{err:#}");
                        show_open_project_error(None, Some(&ws), &message, cx);
                    }
                }
            } else {
                // active_window points at something that is neither a
                // Workspace nor a MultiWorkspace. The classic case on Android
                // (first-boot Open Project): the runtime picker ran in an
                // ExtraWindowActivity, the user dismissed it, and the active
                // window slot still points at that dismissed picker entity
                // when the SAF Activity-result fires. Without this fallback
                // the URI is dropped silently and the user sees "tapping Open
                // Project does nothing until I close + reopen the app".
                //
                let message = "No active Zdroid workspace is available for the selected project.";
                error!("zed_android: {message}");
                show_open_project_error(None, None, message, cx);
            }
        })
        .detach();
    });

    // ImportFromSdcard action removed in L9 — the menu entry was redundant
    // with the existing SAF picker (Open / Add Folder to Project), and the
    // copy-into-`~/projects` flow it ran is now triggered from the noexec
    // banner's confirmation dialog (see title_bar.rs::render_noexec_banner)
    // when an opened worktree turns out to live on a noexec mount.

    // Mirror production zed/src/main.rs's first-launch decision. Both
    // helpers internally call `Workspace::new_local` → `cx.open_window`,
    // construct the Project, build Workspace + MultiWorkspace, and run
    // their own `init` callback. We don't need to do any of that
    // manually; `observe_new` above attaches the project panel.
    let kvp = KeyValueStore::global(cx);
    if matches!(kvp.read_kvp(onboarding::FIRST_OPEN), Ok(None)) {
        info!("zed_android: first launch → show_onboarding_view");
        onboarding::show_onboarding_view(app_state.clone(), cx).detach_and_log_err(cx);
    } else {
        info!("zed_android: returning launch -> restore most recent workspace");
        let db = WorkspaceDb::global(cx);
        let fs = app_state.fs.clone();
        cx.spawn(async move |cx| {
            let recent = workspace::last_opened_workspace_location(&db, fs.as_ref()).await;
            if let Some((workspace_id, location, paths)) = recent {
                let serialized = cx.update(|cx| {
                    workspace::read_serialized_multi_workspaces(
                        vec![SessionWorkspace {
                            workspace_id,
                            location,
                            paths,
                            window_id: None,
                        }],
                        cx,
                    )
                    .into_iter()
                    .next()
                });
                if let Some(serialized) = serialized {
                    match workspace::restore_multiworkspace(serialized, app_state.clone(), cx).await
                    {
                        Ok(_) => return Ok(()),
                        Err(error) => {
                            error!("zed_android: failed to restore recent workspace: {error:#}");
                        }
                    }
                }
            }

            cx.update(|cx| {
                open_new(
                    Default::default(),
                    app_state,
                    cx,
                    |_workspace, _window, _cx| {},
                )
            })
            .await?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    Ok(())
}
