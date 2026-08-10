//! Non-root glibc runtime backed by PRoot-Distro.
//!
//! This adapter intentionally delegates OCI pulling, extraction, hard-link
//! emulation, session tracking, and process-tree termination to PRoot-Distro.
//! Zdroid owns selection and environment integration; it does not reimplement
//! a container manager or silently install a rootfs from an agent prompt.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use util::env::EnvOp;

use crate::config::{ManagedLinuxConfig, RuntimeId};
use crate::health::{HealthStatus, ProgressSink};
use crate::port::{RuntimeProvider, SpawnHandle, SpawnRequest};

const CONTAINER_NAME_MAX: usize = 80;

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
        let mut command = Command::new(program);
        command.env_clear();
        command.envs(self.host_environment());
        let termux_exec = self.config.bootstrap_prefix.join("lib/libtermux-exec.so");
        if termux_exec.is_file() {
            command.env("LD_PRELOAD", termux_exec);
        }
        command
    }

    fn supports_managed_features(&self) -> bool {
        let Ok(output) = self
            .host_command(&self.proot_distro())
            .arg("--help")
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let mut help = String::from_utf8_lossy(&output.stdout).into_owned();
        help.push_str(&String::from_utf8_lossy(&output.stderr));
        help.contains("install") && help.contains("ps") && help.contains("kill")
    }

    fn guest_path(&self, path: &Path) -> PathBuf {
        if let Ok(relative) = path.strip_prefix(self.host_home()) {
            return Path::new("/root").join(relative);
        }
        if let Ok(relative) = path.strip_prefix("/storage/emulated/0") {
            return Path::new("/storage/emulated/0").join(relative);
        }
        path.to_path_buf()
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
        let needs_install_or_upgrade =
            !self.proot_distro().is_file() || !self.supports_managed_features();
        if !needs_install_or_upgrade {
            return Ok(());
        }
        progress.step("Installing or upgrading the Managed Linux runtime engine");
        let pkg = self.config.bootstrap_prefix.join(".zed/bin/pkg");
        if !pkg.is_file() {
            bail!(
                "Zdroid Bootstrap is required before Managed Linux can be installed; missing {}",
                pkg.display()
            );
        }
        let status = self
            .host_command(&pkg)
            .args(["install", "-y", "proot-distro"])
            .status()
            .context("run pkg install proot-distro")?;
        if !status.success() {
            bail!("pkg install proot-distro failed with {status}");
        }
        if !self.proot_distro().is_file() {
            bail!("proot-distro installation completed but its executable is missing");
        }
        if !self.supports_managed_features() {
            bail!(
                "the installed proot-distro is too old for OCI images and managed services; run pkg update and pkg upgrade, then retry"
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
        progress.step(&format!("Downloading ARM64 Linux image {image}"));
        progress.progress(0, 0);
        let status = self
            .host_command(&self.proot_distro())
            .args([
                "install",
                image,
                "--name",
                container,
                "--architecture",
                "aarch64",
            ])
            .status()
            .context("install Managed Linux container")?;
        if !status.success() {
            bail!("proot-distro install failed with {status}");
        }
        if self.rootfs_for(container).is_none() {
            bail!("Managed Linux install completed but container '{container}' has no rootfs");
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
        let status = self
            .host_command(&self.proot_distro())
            .args([
                "login",
                "--shared-home",
                "--detach",
                container,
                "--",
                program,
            ])
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
        if !self.supports_managed_features() {
            return HealthStatus::Misconfigured {
                reason: "PRoot-Distro must be upgraded before Managed Linux can use OCI images and tracked background services.".into(),
            };
        }
        if self.rootfs().is_none() || self.rootfs_for(&self.config.musl_container).is_none() {
            return HealthStatus::NotInstalled {
                hint: format!(
                    "Managed Linux compatibility containers are incomplete (glibc {}, musl {}).",
                    self.config.image, self.config.musl_image
                ),
            };
        }
        HealthStatus::Healthy
    }

    fn install(&self, progress: &mut dyn ProgressSink) -> Result<()> {
        self.install_host_dependency(progress)?;
        self.install_container(&self.config.container, &self.config.image, progress)?;
        self.install_container(
            &self.config.musl_container,
            &self.config.musl_image,
            progress,
        )?;
        progress.step("Verifying Managed Linux runtime");
        let output = self
            .host_command(&self.proot_distro())
            .args([
                "login",
                "--shared-home",
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
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login").arg("--shared-home");
        if let Some(cwd) = req.cwd.as_deref() {
            command.arg("--work-dir").arg(self.guest_path(cwd));
        }
        for key in ["TERM", "COLORTERM", "LANG", "GIT_ASKPASS", "SSH_AUTH_SOCK"] {
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

        for (index, fd) in req.stdio.iter().enumerate() {
            // SAFETY: SpawnRequest stdio values are live inherited descriptors.
            // We immediately duplicate each one, so BorrowedFd never outlives it.
            let owned = unsafe { BorrowedFd::borrow_raw(*fd) }.try_clone_to_owned()?;
            match index {
                0 => command.stdin(Stdio::from(owned)),
                1 => command.stdout(Stdio::from(owned)),
                2 => command.stderr(Stdio::from(owned)),
                _ => unreachable!(),
            };
        }
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
        path.push(std::env::var_os("PATH").unwrap_or_default());
        vec![
            ("HOME".into(), EnvOp::Set(self.host_home().into_os_string())),
            (
                "TMPDIR".into(),
                EnvOp::Set(data_path.join("tmp").into_os_string()),
            ),
            ("TERM".into(), EnvOp::Set(OsString::from("xterm-256color"))),
            ("COLORTERM".into(), EnvOp::Set(OsString::from("truecolor"))),
            ("LANG".into(), EnvOp::Set(OsString::from("en_US.UTF-8"))),
            ("LD_PRELOAD".into(), EnvOp::Remove),
            ("PATH".into(), EnvOp::Set(path)),
            (
                "SHELL".into(),
                EnvOp::Set(data_path.join("bin/zd-exec").into_os_string()),
            ),
        ]
    }

    fn env_for_terminal(&self, _data_path: &Path) -> Vec<(String, EnvOp)> {
        Vec::new()
    }

    fn terminal_shell(&self, data_path: &Path) -> Option<PathBuf> {
        Some(data_path.join("bin/zd-exec"))
    }

    fn workspace_root(&self, _data_path: &Path) -> Option<PathBuf> {
        Some(self.host_home())
    }
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
