//! JNI bridge that mirrors Android's active DNS servers into app-private
//! storage for native Linux-style CLIs.
//!
//! Static musl/Bun binaries cannot use Android's netd resolver and normally
//! read the compile-time path `/etc/resolv.conf`, which Android does not have.
//! Zdroid patches that 16-byte literal to `/proc/self/fd/9`; each launcher
//! opens the private resolver file as descriptor 9 before executing the CLI.
//! This keeps DNS independent of shared-storage permissions.

use android_activity::AndroidApp;
use anyhow::{Context, Result};
use jni::{JavaVM, objects::JObject, objects::JString};

const RESOLV_CONF_FILE_NAME: &str = "zdroid-resolv.conf";
const FALLBACK_NAMESERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];

/// Refresh the resolver file from Android's currently active network.
pub fn populate_resolv_conf(android_app: &AndroidApp) {
    match populate_inner(android_app) {
        Ok((path, servers)) => log::info!(
            "dns_bridge: wrote {} ({} nameserver{} from Android)",
            path.display(),
            servers,
            if servers == 1 { "" } else { "s" }
        ),
        Err(err) => log::warn!("dns_bridge: populate_resolv_conf failed: {err:#}"),
    }
}

fn populate_inner(android_app: &AndroidApp) -> Result<(std::path::PathBuf, usize)> {
    let servers = query_android_dns(android_app).unwrap_or_default();
    let nameservers: Vec<String> = if servers.is_empty() {
        log::info!(
            "dns_bridge: ConnectivityManager returned no DNS servers; \
             falling back to public DNS"
        );
        FALLBACK_NAMESERVERS.iter().map(|s| s.to_string()).collect()
    } else {
        servers
    };

    let data_path = android_app
        .internal_data_path()
        .context("Android internal data path is unavailable")?;
    std::fs::create_dir_all(&data_path)
        .with_context(|| format!("create {}", data_path.display()))?;
    let path = data_path.join(RESOLV_CONF_FILE_NAME);
    let temporary_path = data_path.join(format!("{RESOLV_CONF_FILE_NAME}.tmp"));

    let mut content = String::new();
    for ns in &nameservers {
        content.push_str("nameserver ");
        content.push_str(ns);
        content.push('\n');
    }
    std::fs::write(&temporary_path, content.as_bytes())
        .with_context(|| format!("write {}", temporary_path.display()))?;
    std::fs::rename(&temporary_path, &path)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok((path, nameservers.len()))
}

fn query_android_dns(android_app: &AndroidApp) -> Result<Vec<String>> {
    let vm = unsafe { JavaVM::from_raw(android_app.vm_as_ptr().cast())? };
    let mut env = vm
        .attach_current_thread()
        .context("attach_current_thread for dns query")?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as _) };
    let result = env
        .call_method(
            &activity,
            "getActiveDnsServers",
            "()Ljava/lang/String;",
            &[],
        )
        .context("MainActivity.getActiveDnsServers")?;
    let result_obj = result
        .l()
        .context("getActiveDnsServers returned non-object")?;
    if result_obj.is_null() {
        return Ok(Vec::new());
    }
    let jstr: JString = result_obj.into();
    let csv: String = env
        .get_string(&jstr)
        .context("decode getActiveDnsServers result")?
        .into();
    Ok(csv
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}
