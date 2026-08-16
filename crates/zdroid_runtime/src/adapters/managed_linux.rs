//! Non-root glibc runtime backed by PRoot-Distro.
//!
//! This adapter intentionally delegates OCI pulling, extraction, hard-link
//! emulation, session tracking, and process-tree termination to PRoot-Distro.
//! Zdroid owns selection and environment integration; it does not reimplement
//! a container manager or silently install a rootfs from an agent prompt.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use util::env::EnvOp;

use crate::config::{ManagedLinuxConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

const CONTAINER_NAME_MAX: usize = 80;
const MIN_INSTALL_FREE_BYTES: u64 = 1536 * 1024 * 1024;
const MAX_INSTALL_OUTPUT_BYTES: usize = 256 * 1024;
const BRIDGE_MARKER: &str = "# ZDROID_BOOTSTRAP_COMMAND_BRIDGE_V1";
const HOST_TO_GUEST_MARKER: &str = "# ZDROID_MANAGED_LINUX_COMMAND_BRIDGE_V1";

fn stream_install_output(
    mut reader: impl Read + Send + 'static,
    sender: std::sync::mpsc::SyncSender<io::Result<String>>,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if sender
                        .send(Ok(String::from_utf8_lossy(&buffer[..read]).into_owned()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
}

pub struct ManagedLinuxAdapter {
    config: ManagedLinuxConfig,
}

impl ManagedLinuxAdapter {
    pub fn new(config: ManagedLinuxConfig) -> Result<Self> {
        validate_container_name(&config.container)?;
        validate_container_name(&config.musl_container)?;
        if config.image.trim().is_empty() || config.image.chars().any(char::is_whitespace) {
            bail!("managed Linux image must be a non-empty OCI reference without whitespace");
        }
        if config.musl_image.trim().is_empty() || config.musl_image.chars().any(char::is_whitespace)
        {
            bail!("managed musl image must be a non-empty OCI reference without whitespace");
        }
        Ok(Self { config })
    }

    fn proot_distro(&self) -> PathBuf {
        self.config.bootstrap_prefix.join("bin/proot-distro")
    }

    fn host_home(&self) -> PathBuf {
        self.config
            .bootstrap_prefix
            .parent()
            .map(|files| files.join("home"))
            .unwrap_or_else(|| self.config.bootstrap_prefix.join("home"))
    }

    fn rootfs_for(&self, container: &str) -> Option<PathBuf> {
        let runtime = self.config.bootstrap_prefix.join("var/lib/proot-distro");
        let current = runtime.join("containers").join(container).join("rootfs");
        if current.join("bin/sh").is_file() {
            return Some(current);
        }
        let legacy = runtime.join("installed-rootfs").join(container);
        legacy.join("bin/sh").is_file().then_some(legacy)
    }

    fn rootfs(&self) -> Option<PathBuf> {
        self.rootfs_for(&self.config.container)
    }

    fn remove_incomplete_container_state(&self, container: &str) -> Result<()> {
        if self.rootfs_for(container).is_some() {
            return Ok(());
        }
        let runtime = self.config.bootstrap_prefix.join("var/lib/proot-distro");
        for path in [
            runtime.join("containers").join(container),
            runtime.join("installed-rootfs").join(container),
        ] {
            if path.exists() {
                std::fs::remove_dir_all(&path).with_context(|| {
                    format!(
                        "remove incomplete Managed Linux state at {}",
                        path.display()
                    )
                })?;
            }
        }
        Ok(())
    }

    fn host_environment(&self) -> Vec<(String, OsString)> {
        let prefix = &self.config.bootstrap_prefix;
        let home = self.host_home();
        let mut path = OsString::new();
        path.push(prefix.join(".zed/bin"));
        path.push(":");
        path.push(prefix.join("bin"));
        path.push(":/system/bin:/system/xbin");
        vec![
            ("PREFIX".into(), prefix.as_os_str().to_owned()),
            ("TERMUX__PREFIX".into(), prefix.as_os_str().to_owned()),
            ("TERMUX__HOME".into(), home.into_os_string()),
            (
                "TERMUX__ROOTFS".into(),
                prefix.parent().unwrap_or(prefix).as_os_str().to_owned(),
            ),
            (
                "TERMUX_APP__PACKAGE_NAME".into(),
                OsString::from("com.zdroid"),
            ),
            ("TERMUX_VERSION".into(), OsString::from("zdroid")),
            ("TMPDIR".into(), prefix.join("tmp").into_os_string()),
            ("PATH".into(), path),
            ("HOME".into(), self.host_home().into_os_string()),
            ("LANG".into(), OsString::from("en_US.UTF-8")),
            ("TERM".into(), OsString::from("xterm-256color")),
        ]
    }

    fn host_command(&self, program: &Path) -> Command {
        // Upstream Termux packages commonly ship scripts whose shebang is
        // hardcoded to /data/data/com.termux. LD_PRELOAD cannot rewrite the
        // interpreter used for the kernel's initial script exec, so launch
        // these scripts through the matching interpreter in Zdroid's prefix.
        let script_interpreter =
            bootstrap_script_interpreter(program, &self.config.bootstrap_prefix);
        let mut command = if let Some(interpreter) = script_interpreter {
            let mut command = Command::new(interpreter);
            command.arg(program);
            command
        } else {
            Command::new(program)
        };
        command.env_clear();
        command.envs(self.host_environment());
        let termux_exec = self.config.bootstrap_prefix.join("lib/libtermux-exec.so");
        if termux_exec.is_file() {
            command.env("LD_PRELOAD", termux_exec);
        }
        command
    }

    fn command_help(&self, command: Option<&str>) -> Option<String> {
        let mut invocation = self.host_command(&self.proot_distro());
        if let Some(command) = command {
            invocation.arg(command);
        }
        let Ok(output) = invocation.arg("--help").output() else {
            return None;
        };
        if !output.status.success() {
            return None;
        }
        let mut help = String::from_utf8_lossy(&output.stdout).into_owned();
        help.push_str(&String::from_utf8_lossy(&output.stderr));
        Some(help)
    }

    fn proot_distro_ready(&self) -> bool {
        if !self.proot_distro().is_file() {
            return false;
        }
        let python = self.config.bootstrap_prefix.join("bin/python");
        if !python.is_file() {
            return false;
        }
        let import_ok = self
            .host_command(&python)
            .args(["-c", "import proot_distro"])
            .status()
            .is_ok_and(|status| status.success());
        import_ok && self.command_help(None).is_some()
    }

    fn run_package_command(
        &self,
        program: &Path,
        args: &[&str],
        operation: &str,
        progress: &mut dyn ProgressSink,
    ) -> Result<()> {
        const MAX_LOCK_RETRIES: usize = 300;

        for attempt in 0..=MAX_LOCK_RETRIES {
            let output = self
                .host_command(program)
                .env("DEBIAN_FRONTEND", "noninteractive")
                .args(args)
                .output()
                .with_context(|| format!("run {operation}"))?;
            if output.status.success() {
                return Ok(());
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stderr.trim(), stdout.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let lock_busy = details.contains("Could not get lock")
                || details.contains("Unable to acquire the dpkg frontend lock")
                || details.contains("is another process using it")
                || details.contains("Could not open lock file");
            if lock_busy && attempt < MAX_LOCK_RETRIES {
                progress.step("Waiting for essential package setup to finish");
                progress.progress(0, 0);
                thread::sleep(Duration::from_secs(2));
                continue;
            }

            bail!(
                "{operation} failed with {}{}",
                output.status,
                if details.is_empty() {
                    String::new()
                } else {
                    format!(": {details}")
                }
            );
        }
        unreachable!("package command retry loop always returns")
    }

    pub fn install_bootstrap_command_bridges(&self) -> Result<()> {
        for rootfs in self.installed_rootfs_paths() {
            for (name, target) in [
                ("codex", ".zed/bin/codex"),
                ("claude", ".zed/bin/claude"),
                ("gh", ".zed/bin/gh"),
            ] {
                self.install_guest_bridge_wrapper(&rootfs, name, target)?;
            }
        }
        Ok(())
    }

    pub fn install_host_command_bridges(&self) -> Result<()> {
        if self.rootfs_for(&self.config.container).is_none() {
            return Ok(());
        }

        let data_path = self.environment_root();
        let zd_exec = data_path.join("bin/zd-exec");
        let bridge_dir = self.config.bootstrap_prefix.join(".zed/bin");
        std::fs::create_dir_all(&bridge_dir).with_context(|| {
            format!(
                "create Managed Linux host bridge dir {}",
                bridge_dir.display()
            )
        })?;

        for (name, target) in [
            ("python", "python3"),
            ("python3", "python3"),
            ("pip", "python3 -m pip"),
            ("pip3", "python3 -m pip"),
            ("uv", "uv"),
            ("graphify", "graphify"),
        ] {
            self.install_host_bridge_wrapper(&bridge_dir, &zd_exec, name, target)?;
        }
        Ok(())
    }

    fn install_host_bridge_wrapper(
        &self,
        bridge_dir: &Path,
        zd_exec: &Path,
        name: &str,
        target: &str,
    ) -> Result<()> {
        let path = bridge_dir.join(name);
        if path.exists() {
            let managed = std::fs::read_to_string(&path)
                .map(|contents| contents.contains(HOST_TO_GUEST_MARKER))
                .unwrap_or(false);
            if !managed {
                log::warn!(
                    "Not replacing user-managed Bootstrap launcher at {}",
                    path.display()
                );
                return Ok(());
            }
        }

        let script = format!(
            r#"#!/system/bin/sh
{HOST_TO_GUEST_MARKER}

zd_exec="{zd_exec}"
if [ ! -x "$zd_exec" ]; then
    printf '%s\n' "Zdroid-B: Managed Linux launcher is not installed yet." >&2
    printf '%s\n' "Finish Zdroid-B setup, then retry {name}." >&2
    exit 127
fi

exec "$zd_exec" {target} "$@"
"#,
            name = name,
            target = target,
            zd_exec = zd_exec.display()
        );
        let temporary = path.with_extension("zdroid-tmp");
        std::fs::write(&temporary, script)
            .with_context(|| format!("write Bootstrap host bridge {}", temporary.display()))?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod Bootstrap host bridge {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("publish Bootstrap host bridge {}", path.display()))?;
        Ok(())
    }

    fn install_guest_development_packages(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        if self.rootfs_for(&self.config.container).is_none() {
            return Ok(());
        }

        progress.step("Installing Ubuntu development packages");
        progress.progress(0, 0);
        self.run_ubuntu_shell(
            "export DEBIAN_FRONTEND=noninteractive; apt-get update && apt-get install -y ca-certificates curl git python3-full python3-dev python3-pip python3-venv python3.12 python3.12-dev python3.12-venv pipx build-essential pkg-config",
            "install Ubuntu development packages",
        )?;
        Ok(())
    }

    pub fn repair_python_tools(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        if self.rootfs_for(&self.config.container).is_none() {
            bail!("Ubuntu is not installed; install Zdroid Full Linux first");
        }

        self.install_guest_development_packages(progress)?;
        progress.step("Resetting broken pipx environments");
        progress.progress(0, 0);
        self.run_ubuntu_shell(
            r#"set -eu
mkdir -p "$HOME/.local/share/pipx/broken"
stamp="$(date +%s)-$$"
for path in "$HOME/.local/share/pipx/shared" "$HOME/.local/share/pipx/venvs/graphifyy" "$HOME/.local/share/pipx/trash"; do
    if [ -e "$path" ]; then
        base="$(basename "$path")"
        mv "$path" "$HOME/.local/share/pipx/broken/${base}-${stamp}" 2>/dev/null || rm -rf "$path" || true
    fi
done
rm -rf /tmp/zdroid-python-venv-test
python3.12 -m venv /tmp/zdroid-python-venv-test
/tmp/zdroid-python-venv-test/bin/python -m ensurepip --upgrade
rm -rf /tmp/zdroid-python-venv-test
pipx ensurepath || true
"#,
            "repair Ubuntu Python CLI tools",
        )?;
        self.install_host_command_bridges()?;
        progress.progress(1, 1);
        Ok(())
    }

    pub fn python_tools_health_check(&self) -> HealthStatus {
        if self.rootfs_for(&self.config.container).is_none() {
            return HealthStatus::NotInstalled {
                hint: "Ubuntu is required before Python CLI tools can be repaired.".into(),
            };
        }
        match self.run_ubuntu_shell(
            "command -v python3.12 >/dev/null && command -v pipx >/dev/null && python3.12 -c 'import ensurepip, venv'",
            "check Ubuntu Python CLI tools",
        ) {
            Ok(()) => HealthStatus::Healthy,
            Err(error) => HealthStatus::Misconfigured {
                reason: format!(
                    "Python CLI tools need repair: {error:#}. This fixes pipx, venv and native build dependencies."
                ),
            },
        }
    }

    fn run_ubuntu_shell(&self, script: &str, operation: &str) -> Result<()> {
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
        self.add_bootstrap_bridge_bind_options(&mut command);
        let output = command
            .args([
                self.config.container.as_str(),
                "--",
                "/bin/sh",
                "-lc",
                script,
            ])
            .output()
            .with_context(|| operation.to_string())?;
        if !output.status.success() {
            bail!(
                "{operation} failed: {}{}",
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn installed_rootfs_paths(&self) -> Vec<PathBuf> {
        let runtime = self.config.bootstrap_prefix.join("var/lib/proot-distro");
        let mut rootfs_paths = Vec::new();
        for root in [runtime.join("containers"), runtime.join("installed-rootfs")] {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let current = entry.path().join("rootfs");
                if current.join("bin/sh").is_file() {
                    rootfs_paths.push(current);
                } else if entry.path().join("bin/sh").is_file() {
                    rootfs_paths.push(entry.path());
                }
            }
        }
        rootfs_paths.sort();
        rootfs_paths.dedup();
        rootfs_paths
    }

    fn install_guest_bridge_wrapper(&self, rootfs: &Path, name: &str, target: &str) -> Result<()> {
        let path = rootfs.join("usr/local/bin").join(name);
        if path.exists() {
            let managed = std::fs::read_to_string(&path)
                .map(|contents| contents.contains(BRIDGE_MARKER))
                .unwrap_or(false);
            if !managed {
                log::warn!(
                    "Not replacing user-managed Managed Linux launcher at {}",
                    path.display()
                );
                return Ok(());
            }
        }

        let parent = path
            .parent()
            .context("Managed Linux bridge wrapper has no parent")?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!("create Managed Linux bridge directory {}", parent.display())
        })?;

        let bootstrap_tool = self.config.bootstrap_prefix.join(target);
        let script = format!(
            r#"#!/bin/sh
{BRIDGE_MARKER}

tool="{tool}"
if [ ! -x "$tool" ]; then
    printf '%s\n' "Zdroid-B: Bootstrap {name} is not installed yet." >&2
    printf '%s\n' "Open the Agent panel info actions or install/sign in from the Zdroid-B terminal first." >&2
    exit 127
fi

exec /bin/sh "$tool" "$@"
"#,
            name = name,
            tool = bootstrap_tool.display()
        );
        let temporary = path.with_extension("zdroid-tmp");
        std::fs::write(&temporary, script).with_context(|| {
            format!(
                "write temporary Managed Linux bridge {}",
                temporary.display()
            )
        })?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755)).with_context(
            || {
                format!(
                    "chmod temporary Managed Linux bridge {}",
                    temporary.display()
                )
            },
        )?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("publish Managed Linux bridge {}", path.display()))?;
        Ok(())
    }

    fn supports_oci_install(&self) -> bool {
        self.command_help(Some("install"))
            .is_some_and(|help| help.contains("--name") && help.contains("--architecture"))
    }

    fn supports_shared_home(&self) -> bool {
        self.command_help(Some("login"))
            .is_some_and(|help| help.contains("--shared-home"))
    }

    fn supports_service_tracking(&self) -> bool {
        self.command_help(None)
            .is_some_and(|help| help.contains("ps") && help.contains("kill"))
    }

    fn add_shared_home_option(&self, command: &mut Command) {
        if self.supports_shared_home() {
            command.arg("--shared-home");
        } else {
            // PRoot-Distro v4 calls the equivalent option --termux-home.
            command.arg("--termux-home");
        }
    }

    fn supports_bind_mount(&self) -> bool {
        self.command_help(Some("login"))
            .is_some_and(|help| help.contains("--bind"))
    }

    fn add_bootstrap_bridge_bind_options(&self, command: &mut Command) {
        if !self.supports_bind_mount() {
            return;
        }

        for path in [self.config.bootstrap_prefix.clone(), self.host_home()] {
            if path.exists() {
                command
                    .arg("--bind")
                    .arg(format!("{}:{}", path.display(), path.display()));
            }
        }
    }

    fn guest_path(&self, path: &Path) -> PathBuf {
        if let Some(relative) = private_app_relative(path, &self.host_home()) {
            return Path::new("/root").join(relative);
        }
        if let Ok(relative) = path.strip_prefix("/storage/emulated/0") {
            return Path::new("/storage/emulated/0").join(relative);
        }
        path.to_path_buf()
    }

    fn should_spawn_on_host(&self, program: &Path) -> bool {
        let Some(relative) = private_app_relative(program, &self.host_home()) else {
            return false;
        };

        matches!(
            relative.to_string_lossy().as_ref(),
            "../usr/bin/node" | "../usr/bin/npm"
        ) || relative
            .strip_prefix(".local/bin")
            .ok()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("zdroid-"))
    }

    fn shell_invokes_host_tool(&self, req: &SpawnRequest) -> bool {
        let program = Path::new(&req.program)
            .file_name()
            .and_then(|name| name.to_str());
        if !matches!(program, Some("bash" | "sh")) {
            return false;
        }

        req.args.iter().any(|argument| {
            let argument = argument.to_string_lossy();
            ["/data/data/", "/data/user/0/"].iter().any(|root| {
                argument.contains(&format!("{root}com.zdroid/files/usr/bin/node"))
                    || argument.contains(&format!("{root}com.zdroid/files/usr/bin/npm"))
                    || argument.contains(&format!("{root}com.zdroid/files/home/.local/bin/zdroid-"))
            })
        })
    }

    fn spawn_on_host(&self, req: SpawnRequest) -> Result<Box<dyn SpawnHandle>> {
        let mut command = self.host_command(Path::new(&req.program));
        command.args(&req.args);
        self.apply_host_request_environment(&mut command, &req.env);
        if let Some(cwd) = req.cwd.as_deref().filter(|cwd| cwd.is_dir()) {
            command.current_dir(cwd);
        } else {
            command.current_dir(self.host_home());
        }
        apply_stdio(&mut command, req.stdio)?;
        let child = command
            .spawn()
            .context("spawn Bootstrap Node or agent launcher from Managed Linux")?;
        Ok(Box::new(ChildHandle { child }))
    }

    fn apply_host_request_environment(
        &self,
        command: &mut Command,
        request_environment: &HashMap<String, OsString>,
    ) {
        command.envs(request_environment);
        // Reassert the Bootstrap boundary after applying caller credentials.
        // This preserves tokens while preventing a guest HOME/PATH from
        // routing Node and agent launchers back into Ubuntu.
        command.envs(self.host_environment());
    }

    fn translated_argument(&self, argument: &OsString) -> OsString {
        let path = Path::new(argument);
        if path.is_absolute() {
            self.guest_path(path).into_os_string()
        } else {
            argument.clone()
        }
    }

    fn install_host_dependency(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        if self.proot_distro_ready() {
            return Ok(());
        }
        let pkg = self.config.bootstrap_prefix.join(".zed/bin/pkg");
        if !pkg.is_file() {
            bail!(
                "Zdroid Bootstrap is required before Managed Linux can be installed; missing {}",
                pkg.display()
            );
        }

        let apt = self.config.bootstrap_prefix.join(".zed/bin/apt");
        if !apt.is_file() {
            bail!("Zdroid package repair tool is missing at {}", apt.display());
        }

        progress.step("Repairing Managed Linux package dependencies");
        progress.progress(0, 0);
        self.run_package_command(
            &apt,
            &[
                "--fix-broken",
                "install",
                "-y",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
            ],
            "repair package dependencies",
            progress,
        )?;

        progress.step("Installing the Managed Linux runtime engine");
        progress.progress(0, 0);
        self.run_package_command(
            &pkg,
            &[
                "install",
                "-y",
                "python",
                "python-pip",
                "proot",
                "proot-distro",
            ],
            "install Managed Linux dependencies",
            progress,
        )?;

        if !self.proot_distro_ready() {
            progress.step("Reinstalling the Managed Linux runtime engine");
            progress.progress(0, 0);
            self.run_package_command(
                &apt,
                &["install", "--reinstall", "-y", "proot-distro"],
                "reinstall proot-distro",
                progress,
            )?;
        }

        if !self.proot_distro_ready() {
            bail!(
                "proot-distro was installed but failed its Python module and command health checks"
            );
        }
        Ok(())
    }

    fn install_container(
        &self,
        container: &str,
        image: &str,
        progress: &mut dyn ProgressSink,
    ) -> Result<()> {
        if self.rootfs_for(container).is_some() {
            return Ok(());
        }
        self.ensure_install_space()?;
        self.remove_incomplete_container_state(container)?;
        progress.step(&format!("Downloading ARM64 Linux image {image}"));
        progress.progress(0, 0);
        let mut command = self.host_command(&self.proot_distro());
        command.arg("install");
        if self.supports_oci_install() {
            command.args([image, "--name", container, "--architecture", "aarch64"]);
        } else {
            let distro = legacy_distro_alias(image)?;
            progress.warn(
                "The bundled repository provides PRoot-Distro v4; installing its verified distribution rootfs instead of pulling an OCI image.",
            );
            if container == distro {
                command.arg(distro);
            } else {
                command.args(["--override-alias", container, distro]);
            }
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("start Managed Linux container installation")?;
        let stdout = child.stdout.take().context("capture proot-distro stdout")?;
        let stderr = child.stderr.take().context("capture proot-distro stderr")?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(32);
        stream_install_output(stdout, sender.clone());
        stream_install_output(stderr, sender.clone());
        drop(sender);

        let mut details = String::new();
        for line in receiver {
            match line {
                Ok(chunk) if !chunk.trim().is_empty() => {
                    let status = chunk
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or(chunk.trim());
                    progress.step(&format!("Installing Linux: {}", status.trim()));
                    progress.progress(0, 0);
                    if details.len() < MAX_INSTALL_OUTPUT_BYTES {
                        let remaining = MAX_INSTALL_OUTPUT_BYTES - details.len();
                        let mut end = chunk.len().min(remaining);
                        while !chunk.is_char_boundary(end) {
                            end -= 1;
                        }
                        details.push_str(&chunk[..end]);
                    }
                }
                Ok(_) => {}
                Err(error) => log::warn!("read proot-distro install output: {error}"),
            }
        }
        let status = child
            .wait()
            .context("wait for Managed Linux container installation")?;
        if !status.success() {
            let details = details.trim();
            bail!(
                "proot-distro install failed with {}{}",
                status,
                if details.is_empty() {
                    String::new()
                } else {
                    format!(": {details}")
                }
            );
        }
        if self.rootfs_for(container).is_none() {
            bail!("Managed Linux install completed but container '{container}' has no rootfs");
        }
        Ok(())
    }

    fn ensure_install_space(&self) -> Result<()> {
        #[cfg(target_os = "android")]
        {
            let stats = nix::sys::statvfs::statvfs(&self.config.bootstrap_prefix)
                .context("check free space for Managed Linux")?;
            let available = stats
                .blocks_available()
                .saturating_mul(stats.fragment_size());
            if available < MIN_INSTALL_FREE_BYTES {
                bail!(
                    "Managed Linux needs at least 1.5 GB free; only {:.1} GB is available",
                    available as f64 / 1_073_741_824.0
                );
            }
        }
        Ok(())
    }

    pub fn start_detached(&self, use_musl: bool, program: &str, args: &[OsString]) -> Result<()> {
        if program.is_empty() {
            bail!("service command must not be empty");
        }
        let container = if use_musl {
            &self.config.musl_container
        } else {
            &self.config.container
        };
        if self.rootfs_for(container).is_none() {
            bail!("Managed Linux container '{container}' is not installed");
        }
        if !self.supports_service_tracking() {
            bail!(
                "background service tracking requires a newer PRoot-Distro; interactive Managed Linux commands remain available"
            );
        }
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
        self.add_bootstrap_bridge_bind_options(&mut command);
        let status = command
            .args(["--detach", container, "--", program])
            .args(args)
            .status()
            .context("start detached Managed Linux service")?;
        if status.success() {
            Ok(())
        } else {
            bail!("Managed Linux service failed to start with {status}")
        }
    }

    /// Run an arbitrary installed OCI container's declared entrypoint in the
    /// background. Container names are validated before they reach
    /// PRoot-Distro, and arguments are forwarded without a shell.
    pub fn run_container_detached(&self, container: &str, args: &[OsString]) -> Result<()> {
        validate_container_name(container)?;
        if self.rootfs_for(container).is_none() {
            bail!("Managed Linux container '{container}' is not installed");
        }
        if !self.supports_service_tracking() {
            bail!("OCI entrypoint services require a newer PRoot-Distro");
        }
        let mut command = self.host_command(&self.proot_distro());
        command.args(["run", container, "--detach"]);
        if !args.is_empty() {
            command.arg("--").args(args);
        }
        let status = command.status().context("run detached OCI container")?;
        if status.success() {
            Ok(())
        } else {
            bail!("Managed Linux container '{container}' failed to start with {status}")
        }
    }

    pub fn list_services(&self) -> Result<()> {
        if !self.supports_service_tracking() {
            bail!("service listing requires a newer PRoot-Distro");
        }
        let status = self
            .host_command(&self.proot_distro())
            .arg("ps")
            .status()
            .context("list Managed Linux services")?;
        if status.success() {
            Ok(())
        } else {
            bail!("proot-distro ps failed with {status}")
        }
    }

    pub fn stop_service(&self, pid: u32) -> Result<()> {
        if !self.supports_service_tracking() {
            bail!("service stopping requires a newer PRoot-Distro");
        }
        let status = self
            .host_command(&self.proot_distro())
            .args(["kill", &pid.to_string()])
            .status()
            .context("stop Managed Linux service")?;
        if status.success() {
            Ok(())
        } else {
            bail!("proot-distro kill failed with {status}")
        }
    }

    pub fn musl_health_check(&self) -> HealthStatus {
        if !self.proot_distro().is_file() {
            return HealthStatus::NotInstalled {
                hint: "Install Managed Linux before adding Alpine musl compatibility.".into(),
            };
        }
        if self.rootfs_for(&self.config.musl_container).is_none() {
            return HealthStatus::NotInstalled {
                hint: format!(
                    "Optional musl compatibility image {} is not installed.",
                    self.config.musl_image
                ),
            };
        }
        HealthStatus::Healthy
    }

    pub fn install_musl(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        self.install_host_dependency(progress)?;
        self.install_container(
            &self.config.musl_container,
            &self.config.musl_image,
            progress,
        )?;
        progress.step("Verifying Alpine musl compatibility");
        self.install_bootstrap_command_bridges()?;
        self.install_host_command_bridges()?;
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
        self.add_bootstrap_bridge_bind_options(&mut command);
        let output = command
            .args([
                self.config.musl_container.as_str(),
                "--",
                "/bin/sh",
                "-c",
                "test -r /etc/alpine-release && uname -m",
            ])
            .output()
            .context("verify Alpine musl compatibility")?;
        if !output.status.success() {
            bail!(
                "Alpine musl verification failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        progress.progress(1, 1);
        Ok(())
    }
}

struct ChildHandle {
    child: std::process::Child,
}

impl SpawnHandle for ChildHandle {
    fn wait(&mut self) -> Result<i32> {
        Ok(self.child.wait()?.code().unwrap_or(-1))
    }

    fn kill(&mut self) -> Result<()> {
        self.child.kill().context("kill Managed Linux process")
    }
}

impl RuntimeProvider for ManagedLinuxAdapter {
    fn id(&self) -> RuntimeId {
        RuntimeId::ManagedLinux
    }

    fn health_check(&self) -> HealthStatus {
        if !self.proot_distro().is_file() {
            return HealthStatus::NotInstalled {
                hint: "Managed Linux engine is not installed. Select Install to add PRoot-Distro explicitly.".into(),
            };
        }
        if !self.proot_distro_ready() {
            return HealthStatus::Misconfigured {
                reason: "Managed Linux package installation is incomplete. Select Repair to restore its Python and PRoot dependencies.".into(),
            };
        }
        if self.rootfs().is_none() {
            return HealthStatus::NotInstalled {
                hint: format!(
                    "Managed Linux glibc environment {} is not installed.",
                    self.config.image
                ),
            };
        }
        HealthStatus::Healthy
    }

    fn install(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        self.install_host_dependency(progress)?;
        self.install_container(&self.config.container, &self.config.image, progress)?;
        self.install_guest_development_packages(progress)?;
        progress.step("Verifying Managed Linux runtime");
        self.install_bootstrap_command_bridges()?;
        self.install_host_command_bridges()?;
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
        self.add_bootstrap_bridge_bind_options(&mut command);
        let output = command
            .args([
                self.config.container.as_str(),
                "--",
                "/bin/sh",
                "-c",
                "test -r /etc/os-release && uname -m",
            ])
            .output()
            .context("verify Managed Linux runtime")?;
        if !output.status.success() {
            bail!(
                "Managed Linux verification failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        progress.progress(1, 1);
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        for container in [&self.config.container, &self.config.musl_container] {
            if self.rootfs_for(container).is_none() {
                continue;
            }
            let status = self
                .host_command(&self.proot_distro())
                .args(["remove", container])
                .status()
                .with_context(|| format!("remove Managed Linux container '{container}'"))?;
            if !status.success() {
                bail!("proot-distro remove '{container}' failed with {status}");
            }
        }
        Ok(())
    }

    fn spawn(&self, req: SpawnRequest) -> Result<Box<dyn SpawnHandle>> {
        if self.rootfs().is_none() {
            bail!(
                "Managed Linux is not installed; open Android Runtime settings and install it first"
            );
        }
        if (Path::new(&req.program).is_absolute()
            && self.should_spawn_on_host(Path::new(&req.program)))
            || self.shell_invokes_host_tool(&req)
        {
            return self.spawn_on_host(req);
        }

        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
        self.add_bootstrap_bridge_bind_options(&mut command);
        if let Some(cwd) = req.cwd.as_deref() {
            command.arg("--work-dir").arg(self.guest_path(cwd));
        }
        for key in [
            "TERM",
            "COLORTERM",
            "LANG",
            "GIT_ASKPASS",
            "SSH_AUTH_SOCK",
            "ZDROID_RUNTIME",
        ] {
            if let Some(value) = req.env.get(key) {
                let mut assignment = OsString::from(key);
                assignment.push("=");
                assignment.push(value);
                command.arg("--env").arg(assignment);
            }
        }
        command.arg(&self.config.container).arg("--");
        command.arg(self.translated_argument(&OsString::from(&req.program)));
        command.args(req.args.iter().map(|arg| self.translated_argument(arg)));

        apply_stdio(&mut command, req.stdio)?;
        let child = command.spawn().context("spawn in Managed Linux")?;
        Ok(Box::new(ChildHandle { child }))
    }

    fn needs_command_bridge(&self) -> bool {
        true
    }

    fn environment_root(&self) -> PathBuf {
        self.config
            .bootstrap_prefix
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.config.bootstrap_prefix.clone())
    }

    fn list_binaries(&self) -> Vec<String> {
        let Some(rootfs) = self.rootfs() else {
            return Vec::new();
        };
        let mut names = BTreeSet::new();
        for sub in ["usr/local/bin", "usr/bin", "usr/sbin", "bin", "sbin"] {
            if let Ok(entries) = std::fs::read_dir(rootfs.join(sub)) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str()
                        && !name.starts_with('.')
                    {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
        names.into_iter().collect()
    }

    fn env_for_zed_process(&self, data_path: &Path) -> Vec<(String, EnvOp)> {
        let mut path = OsString::new();
        path.push(data_path.join("zd-runtime"));
        path.push(":");
        path.push(data_path.join("bin"));
        path.push(":");
        path.push(self.config.bootstrap_prefix.join(".zed/bin"));
        path.push(":");
        path.push(self.config.bootstrap_prefix.join("bin"));
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        vec![
            ("HOME".into(), EnvOp::Set(self.host_home().into_os_string())),
            (
                "TMPDIR".into(),
                EnvOp::Set(data_path.join("tmp").into_os_string()),
            ),
            ("TERM".into(), EnvOp::Set(OsString::from("xterm-256color"))),
            ("COLORTERM".into(), EnvOp::Set(OsString::from("truecolor"))),
            (
                "ZDROID_RUNTIME".into(),
                EnvOp::Set(OsString::from("Ubuntu 24.04 · glibc")),
            ),
            ("LANG".into(), EnvOp::Set(OsString::from("en_US.UTF-8"))),
            ("LD_PRELOAD".into(), EnvOp::Remove),
            ("PATH".into(), EnvOp::Set(path)),
            (
                "SHELL".into(),
                EnvOp::Set(data_path.join("bin/zd-exec").into_os_string()),
            ),
        ]
    }

    fn env_for_terminal(&self, data_path: &Path) -> Vec<(String, EnvOp)> {
        vec![
            (
                "ZDROID_RUNTIME".into(),
                EnvOp::Set(OsString::from("Ubuntu 24.04 · glibc")),
            ),
            (
                "SHELL".into(),
                EnvOp::Set(data_path.join("bin/zd-exec").into_os_string()),
            ),
            ("LD_PRELOAD".into(), EnvOp::Remove),
        ]
    }

    fn terminal_shell(&self, data_path: &Path) -> Option<PathBuf> {
        Some(data_path.join("bin/zd-exec"))
    }

    fn workspace_root(&self, _data_path: &Path) -> Option<PathBuf> {
        Some(self.host_home())
    }
}

fn private_app_relative(path: &Path, host_home: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(host_home) {
        return Some(relative.to_path_buf());
    }

    let files = host_home.parent()?;
    let app_root = files.parent()?;
    let package = app_root.file_name()?;
    for data_root in [Path::new("/data/data"), Path::new("/data/user/0")] {
        let alias_root = data_root.join(package);
        let Ok(app_relative) = path.strip_prefix(alias_root) else {
            continue;
        };
        if let Ok(relative) = app_relative.strip_prefix("files/home") {
            return Some(relative.to_path_buf());
        }
        if let Ok(relative) = app_relative.strip_prefix("files") {
            return Some(Path::new("..").join(relative));
        }
    }
    None
}

fn apply_stdio(command: &mut Command, stdio: [i32; 3]) -> Result<()> {
    for (index, fd) in stdio.into_iter().enumerate() {
        // SAFETY: SpawnRequest stdio values are live inherited descriptors.
        // We immediately duplicate each one, so BorrowedFd never outlives it.
        let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        match index {
            0 => command.stdin(Stdio::from(owned)),
            1 => command.stdout(Stdio::from(owned)),
            2 => command.stderr(Stdio::from(owned)),
            _ => unreachable!(),
        };
    }
    Ok(())
}

fn validate_container_name(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > CONTAINER_NAME_MAX {
        bail!("Managed Linux container name must be 1-{CONTAINER_NAME_MAX} characters");
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-'))
    {
        bail!("Managed Linux container name contains unsupported characters");
    }
    Ok(())
}

fn legacy_distro_alias(image: &str) -> Result<&'static str> {
    if image == "ubuntu" || image.starts_with("ubuntu:") {
        Ok("ubuntu")
    } else if image == "alpine" || image.starts_with("alpine:") {
        Ok("alpine")
    } else {
        bail!(
            "this PRoot-Distro version cannot pull OCI image '{image}'; update PRoot-Distro or choose the built-in Ubuntu/Alpine runtime"
        )
    }
}

fn bootstrap_script_interpreter(program: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut first_line = String::new();
    BufReader::new(File::open(program).ok()?)
        .read_line(&mut first_line)
        .ok()?;
    if !first_line.starts_with("#!") {
        return None;
    }

    let interpreter = if first_line.contains("python") {
        prefix.join("bin/python")
    } else if first_line.contains("bash") {
        prefix.join("bin/bash")
    } else if first_line.contains("/sh") {
        prefix.join("bin/sh")
    } else {
        return None;
    };
    interpreter.is_file().then_some(interpreter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_adapter() -> ManagedLinuxAdapter {
        ManagedLinuxAdapter::new(ManagedLinuxConfig {
            bootstrap_prefix: PathBuf::from("/data/data/com.zdroid/files/usr"),
            container: "ubuntu".into(),
            image: "ubuntu:24.04".into(),
            musl_container: "alpine".into(),
            musl_image: "alpine:3.21".into(),
        })
        .unwrap()
    }

    #[test]
    fn maps_builtin_images_to_legacy_distro_aliases() {
        assert_eq!(legacy_distro_alias("ubuntu:24.04").unwrap(), "ubuntu");
        assert_eq!(legacy_distro_alias("alpine:3.21").unwrap(), "alpine");
    }

    #[test]
    fn rejects_arbitrary_images_for_legacy_proot_distro() {
        assert!(legacy_distro_alias("postgres:17").is_err());
    }

    #[test]
    fn ignores_non_script_files_when_selecting_an_interpreter() {
        let prefix = Path::new("/not-used");
        assert!(bootstrap_script_interpreter(Path::new("/missing"), prefix).is_none());
    }

    #[test]
    fn maps_android_private_path_aliases_to_shared_home() {
        let home = Path::new("/data/data/com.zdroid/files/home");
        assert_eq!(
            private_app_relative(
                Path::new("/data/user/0/com.zdroid/files/home/projects/demo"),
                home,
            ),
            Some(PathBuf::from("projects/demo")),
        );
        assert_eq!(
            private_app_relative(
                Path::new("/data/user/0/com.zdroid/files/usr/bin/node"),
                home,
            ),
            Some(PathBuf::from("../usr/bin/node")),
        );
    }

    #[test]
    fn only_routes_bootstrap_node_npm_and_agent_launchers_to_host() {
        let adapter = test_adapter();
        assert!(
            adapter.should_spawn_on_host(Path::new("/data/user/0/com.zdroid/files/usr/bin/node",))
        );
        assert!(
            adapter.should_spawn_on_host(Path::new("/data/data/com.zdroid/files/usr/bin/npm",))
        );
        assert!(adapter.should_spawn_on_host(Path::new(
            "/data/data/com.zdroid/files/home/.local/bin/zdroid-claude-agent-acp",
        )));
        assert!(
            !adapter.should_spawn_on_host(Path::new("/data/data/com.zdroid/files/usr/bin/git",))
        );
        assert!(!adapter.should_spawn_on_host(Path::new(
            "/data/data/com.zdroid/files/home/projects/demo/run.sh",
        )));
    }

    #[test]
    fn detects_bootstrap_node_inside_shell_command() {
        let adapter = test_adapter();
        let request = SpawnRequest {
            program: "bash".into(),
            args: vec![
                OsString::from("-c"),
                OsString::from("/data/user/0/com.zdroid/files/usr/bin/node /tmp/agent.js"),
            ],
            cwd: None,
            env: HashMap::new(),
            interactive: false,
            stdio: [0, 1, 2],
        };

        assert!(adapter.shell_invokes_host_tool(&request));
    }
}
