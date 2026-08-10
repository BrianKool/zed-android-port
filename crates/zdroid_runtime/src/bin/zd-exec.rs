//! `zd-exec` — generic spawn wrapper. Routes every PATH-resolved
//! invocation (bash, git, rust-analyzer, …) through the configured
//! `RuntimeProvider`.
//!
//! Three invocation shapes:
//!
//!   1. **Symlink** at `$PREFIX/zd-runtime/<name>`. argv[0] basename
//!      *is* the target program name. This is how Zed's `Command::new`
//!      lands here — kernel resolves PATH, finds the symlink, exec's
//!      this binary. We dispatch to `<name>`.
//!
//!   2. **Shell** invocation: `zd-exec` with no positional program, or
//!      with leading `-c`/`-l`-style flags. Happens when alacritty
//!      exec's us as `$SHELL` (`execve(zd-exec, ["zd-exec"], envp)` or
//!      `["zd-exec", "-c", "cmd"]`). We dispatch to `bash` with whatever
//!      flags the caller passed — the integrated terminal lands in the
//!      configured adapter's bash this way.
//!
//!   3. **Direct** target invocation: `zd-exec <program> [args…]`. First
//!      positional is the target binary. Used for testing and one-off
//!      tool invocations from a script.
//!
//! Reads `runtime.toml` from `$PREFIX/etc/zd-runtime.toml` to pick the
//! active adapter, builds a `SpawnRequest` from the current process'
//! cwd / env / stdio, calls `provider.spawn(req).wait()`, and exits
//! with the child's exit code (or `128 + signum` if killed by signal,
//! matching bash semantics).
//!
//! No `su` fallback. If the chroot adapter can't reach `zd-spawnd`,
//! we fail loudly with a hint — silently re-execing through `su` is
//! how the per-spawn fork-bomb regression sneaks back in (see memory:
//! `project_runtime_swap_architecture`).

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zdroid_runtime::RuntimeProvider;
use zdroid_runtime::adapters;
use zdroid_runtime::config::{ManagedLinuxConfig, RuntimeFile};
use zdroid_runtime::elf::{Architecture, BinaryFormat, LibcFamily, inspect_binary};
use zdroid_runtime::health::HealthStatus;
use zdroid_runtime::port::SpawnRequest;

/// Same path the picker writes to. Hardcoded — the wrapper has no way
/// to discover `$PREFIX` other than by reading the env, and we want
/// the wrapper's behavior to be deterministic regardless of who
/// invoked it (Zed, an interactive shell, a daemon).
const RUNTIME_TOML: &str = "/data/data/com.zdroid/files/usr/etc/zd-runtime.toml";

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().collect();
    let argv0 = argv.first().cloned().unwrap_or_default();
    let basename = Path::new(&argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&argv0);

    if basename == "zd-run" {
        return run_compatible_binary(&argv);
    }
    if basename == "zd-service" {
        return manage_service(&argv);
    }

    let (program, prog_args): (String, Vec<OsString>) = if basename == "zd-exec" {
        // Shell-mode dispatch: no args, or argv[1] is a flag (alacritty
        // commonly invokes `$SHELL -c '<cmd>'` for non-interactive
        // task spawns). Forward as bash <flags>.
        match argv.get(1) {
            None => {
                // Login shell so the chrooted bash sources /etc/profile
                // and ~/.profile in addition to ~/.bashrc — debian /
                // kali put `~/.local/bin` and similar on PATH from
                // ~/.profile, which a non-login interactive shell
                // wouldn't pick up. Without -l: claude (and any other
                // user-installed tools) silently disappear from PATH.
                ("bash".to_string(), vec![OsString::from("-l")])
            }
            Some(first) if first.starts_with('-') => {
                let rest = argv.iter().skip(1).map(OsString::from).collect();
                ("bash".to_string(), rest)
            }
            Some(prog) => {
                let rest = argv.iter().skip(2).map(OsString::from).collect();
                (prog.clone(), rest)
            }
        }
    } else {
        let rest = argv.iter().skip(1).map(OsString::from).collect();
        (basename.to_string(), rest)
    };

    let provider = match build_provider() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("zd-exec: {e:#}");
            return ExitCode::from(127);
        }
    };

    let cwd = env::current_dir().ok();
    let env_map: HashMap<String, OsString> = env::vars_os()
        .filter_map(|(k, v)| k.into_string().ok().map(|k| (k, v)))
        .collect();

    let req = SpawnRequest {
        program,
        args: prog_args,
        cwd,
        env: env_map,
        interactive: std::io::stdin().is_terminal(),
        stdio: [0, 1, 2],
    };

    let mut handle = match provider.spawn(req) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("zd-exec: spawn: {e:#}");
            return ExitCode::from(127);
        }
    };

    match handle.wait() {
        Ok(code) if code >= 0 => ExitCode::from(code.min(255) as u8),
        Ok(code) => {
            let signum = (-code).clamp(0, 127) as u8;
            ExitCode::from(128 + signum)
        }
        Err(e) => {
            eprintln!("zd-exec: wait: {e:#}");
            ExitCode::from(127)
        }
    }
}

fn run_compatible_binary(argv: &[String]) -> ExitCode {
    let Some(program) = argv.get(1) else {
        eprintln!("Usage: zd-run <executable> [arguments...]");
        return ExitCode::from(64);
    };
    let requested_path = PathBuf::from(program);
    let path = match requested_path.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("zd-run: cannot open {}: {err}", requested_path.display());
            return ExitCode::from(66);
        }
    };
    let info = match inspect_binary(&path) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("zd-run: cannot inspect {}: {err:#}", path.display());
            return ExitCode::from(65);
        }
    };
    eprintln!("zd-run: detected {}", info.compatibility_summary());

    if let Some(architecture) = info.architecture
        && architecture != Architecture::Aarch64
    {
        eprintln!(
            "zd-run: unsupported architecture {architecture:?}; this APK supports ARM64 executables only"
        );
        return ExitCode::from(126);
    }

    let args: Vec<OsString> = argv.iter().skip(2).map(OsString::from).collect();
    match (&info.format, info.libc) {
        (BinaryFormat::Elf, LibcFamily::Bionic | LibcFamily::None) => run_native(&path, &args),
        (BinaryFormat::Elf, LibcFamily::Glibc) => {
            run_in_managed_linux(&path, &args, "zdroid-linux", "ubuntu:24.04")
        }
        (BinaryFormat::Elf, LibcFamily::Musl) => {
            run_in_managed_linux(&path, &args, "zdroid-musl", "alpine:3.21")
        }
        (BinaryFormat::Script { interpreter }, _) => {
            let native = interpreter.as_deref().is_some_and(|interpreter| {
                interpreter.starts_with("/system/")
                    || interpreter.starts_with("/data/data/com.zdroid/")
            });
            if native {
                run_native(&path, &args)
            } else {
                run_in_managed_linux(&path, &args, "zdroid-linux", "ubuntu:24.04")
            }
        }
        (BinaryFormat::Other, _) => {
            eprintln!(
                "zd-run: {} is neither an ELF executable nor a script with a shebang",
                path.display()
            );
            ExitCode::from(126)
        }
        (BinaryFormat::Elf, LibcFamily::Unknown) => {
            eprintln!(
                "zd-run: unsupported dynamic loader {}; install a matching ARM64 runtime and run the file inside it",
                info.interpreter.as_deref().unwrap_or("<missing>")
            );
            ExitCode::from(126)
        }
    }
}

fn run_native(path: &Path, args: &[OsString]) -> ExitCode {
    match std::process::Command::new(path).args(args).status() {
        Ok(status) => status_to_exit_code(status.code()),
        Err(err) => {
            eprintln!(
                "zd-run: native launch of {} failed: {err}. The binary may require Linux facilities unavailable in Android/Bionic.",
                path.display()
            );
            ExitCode::from(126)
        }
    }
}

fn run_in_managed_linux(path: &Path, args: &[OsString], container: &str, image: &str) -> ExitCode {
    let config = ManagedLinuxConfig {
        bootstrap_prefix: PathBuf::from("/data/data/com.zdroid/files/usr"),
        container: container.into(),
        image: image.into(),
        musl_container: "zdroid-musl".into(),
        musl_image: "alpine:3.21".into(),
    };
    let provider = match adapters::managed_linux::ManagedLinuxAdapter::new(config) {
        Ok(provider) => provider,
        Err(err) => {
            eprintln!("zd-run: invalid Managed Linux configuration: {err:#}");
            return ExitCode::from(78);
        }
    };
    if !matches!(provider.health_check(), HealthStatus::Healthy) {
        eprintln!(
            "zd-run: required runtime '{container}' is not installed. Open Settings > Android Runtime to install Managed Linux. For musl binaries, run `proot-distro install {image} --name {container} --architecture aarch64`."
        );
        return ExitCode::from(69);
    }
    let env_map: HashMap<String, OsString> = env::vars_os()
        .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
        .collect();
    let request = SpawnRequest {
        program: path.to_string_lossy().into_owned(),
        args: args.to_vec(),
        cwd: env::current_dir().ok(),
        env: env_map,
        interactive: std::io::stdin().is_terminal(),
        stdio: [0, 1, 2],
    };
    let mut handle = match provider.spawn(request) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("zd-run: Managed Linux launch failed: {err:#}");
            return ExitCode::from(126);
        }
    };
    match handle.wait() {
        Ok(code) if code >= 0 => ExitCode::from(code.min(255) as u8),
        Ok(code) => ExitCode::from((128 + (-code).clamp(0, 127)) as u8),
        Err(err) => {
            eprintln!("zd-run: wait failed: {err:#}");
            ExitCode::from(126)
        }
    }
}

fn manage_service(argv: &[String]) -> ExitCode {
    let config = ManagedLinuxConfig {
        bootstrap_prefix: PathBuf::from("/data/data/com.zdroid/files/usr"),
        container: "zdroid-linux".into(),
        image: "ubuntu:24.04".into(),
        musl_container: "zdroid-musl".into(),
        musl_image: "alpine:3.21".into(),
    };
    let provider = match adapters::managed_linux::ManagedLinuxAdapter::new(config) {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("zd-service: invalid Managed Linux configuration: {error:#}");
            return ExitCode::from(78);
        }
    };
    let result = match argv.get(1).map(String::as_str) {
        Some("list") => provider.list_services(),
        Some("stop") => match argv.get(2).and_then(|value| value.parse::<u32>().ok()) {
            Some(pid) if pid > 0 => provider.stop_service(pid),
            _ => {
                eprintln!("Usage: zd-service stop <positive-pid>");
                return ExitCode::from(64);
            }
        },
        Some("run") => {
            let Some(container) = argv.get(2) else {
                eprintln!("Usage: zd-service run <container> [arguments...]");
                return ExitCode::from(64);
            };
            let args: Vec<OsString> = argv.iter().skip(3).map(OsString::from).collect();
            provider.run_container_detached(container, &args)
        }
        Some("start") => {
            let mut index = 2;
            let use_musl = argv.get(index).is_some_and(|value| value == "--musl");
            if use_musl {
                index += 1;
            }
            if argv.get(index).is_some_and(|value| value == "--") {
                index += 1;
            }
            let Some(program) = argv.get(index) else {
                eprintln!("Usage: zd-service start [--musl] -- <program> [arguments...]");
                return ExitCode::from(64);
            };
            let args: Vec<OsString> = argv.iter().skip(index + 1).map(OsString::from).collect();
            provider.start_detached(use_musl, program, &args)
        }
        _ => {
            eprintln!(
                "Usage: zd-service <start [--musl] -- PROGRAM [ARGS...]|run CONTAINER [ARGS...]|list|stop PID>"
            );
            return ExitCode::from(64);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zd-service: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn status_to_exit_code(code: Option<i32>) -> ExitCode {
    ExitCode::from(code.unwrap_or(1).clamp(0, 255) as u8)
}

fn build_provider() -> anyhow::Result<Box<dyn zdroid_runtime::port::RuntimeProvider>> {
    let path = PathBuf::from(RUNTIME_TOML);
    let file = RuntimeFile::load(&path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} not found. Open Zdroid and pick a runtime in Settings first.",
            path.display(),
        )
    })?;
    let resolved = file.resolve()?;
    adapters::for_config(&resolved)
}
