//! Non-root glibc runtime backed by PRoot-Distro.
//!
//! This adapter intentionally delegates OCI pulling, extraction, hard-link
//! emulation, session tracking, and process-tree termination to PRoot-Distro.
//! Zdroid owns selection and environment integration; it does not reimplement
//! a container manager or silently install a rootfs from an agent prompt.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::BorrowedFd;
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
        let output = command
            .output()
            .context("install Managed Linux container")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stderr.trim(), stdout.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            bail!(
                "proot-distro install failed with {}{}",
                output.status,
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
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
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
        progress.step("Verifying Managed Linux runtime");
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
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
        let mut command = self.host_command(&self.proot_distro());
        command.arg("login");
        self.add_shared_home_option(&mut command);
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
}
