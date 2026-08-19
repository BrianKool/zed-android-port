//! Standalone Android-native helper for Zed's SSH_ASKPASS flow.
//!
//! On desktop, Zed sets `ASKPASS_PROGRAM` (in `crates/askpass/src/askpass.rs`)
//! to `current_exe()` — i.e. the same `zed` binary, invoked with `--askpass=
//! <socket>`. The askpass.sh shim pipes ssh's prompt into that invocation,
//! the binary connects to a unix socket back to the running Zed, the main
//! process pops a UI modal asking for the password, and the password flows
//! back through the socket and onto stdout where ssh reads it.
//!
//! On Android, `current_exe()` resolves to `/system/bin/app_process64` (the
//! Zygote / Dalvik launcher that hosts the APK's main DEX runtime). When
//! ssh execs that binary outside of an Activity context, Android aborts
//! with `Error changing dalvik-cache ownership: Permission denied` —
//! `untrusted_app_27` SELinux can't re-spawn another instance of the
//! Zygote. The askpass attempt SIGABRTs three times, ssh treats it as
//! three failed password tries, and connection ends with `Permission
//! denied (publickey,password)` even though the user never got a chance
//! to type anything.
//!
//! This helper is the missing piece: a tiny, regular ELF (not Zygote /
//! not Dalvik / not app_process) that does the same socket-IPC dance the
//! askpass crate's `main()` does on desktop. It gets bundled in the APK
//! as an asset, extracted to `$PREFIX/.zed/bin/zed-askpass-helper` at boot,
//! and wired in via `askpass::set_askpass_program` so any subsequent
//! AskPassSession uses this binary instead of `current_exe()`.
//!
//! Protocol (kept identical to `crates/askpass/src/askpass.rs::main` so
//! the existing socket server in the gpui process Just Works):
//!
//!   1. Parse `--askpass=<unix-socket-path>` from argv.
//!   2. Read all of stdin into a buffer (this is the prompt ssh wants
//!      answered, e.g. `"<user>@<host>'s password: "`).
//!   3. NUL-terminate the buffer if it isn't already (the desktop main
//!      function does this — keep the byte stream identical).
//!   4. Connect to the unix socket. Write the prompt buffer.
//!   5. Read the socket until EOF — this is the password (or
//!      passphrase) the user typed into Zed's modal.
//!   6. Write that response to stdout. ssh reads stdout and treats it
//!      as the password.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(socket) = env::args()
        .find_map(|argument| argument.strip_prefix("--git-credential=").map(String::from))
    {
        return git_credential(&socket);
    }

    let socket_argument = env::args().find_map(|a| a.strip_prefix("--askpass=").map(String::from));
    let direct_mode = socket_argument.is_none();
    let socket = socket_argument.or_else(|| env::var("ZED_ASKPASS_SOCKET").ok());

    if socket.as_deref().is_none_or(str::is_empty) {
        return github_askpass_fallback();
    }
    let socket = socket.expect("non-empty askpass socket checked above");

    let mut stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("zed-askpass-helper: connect {socket}: {err}");
            return ExitCode::from(1);
        }
    };

    let mut prompt = if direct_mode {
        env::args()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\0")
            .into_bytes()
    } else {
        let mut prompt = Vec::new();
        if let Err(err) = io::stdin().read_to_end(&mut prompt) {
            eprintln!("zed-askpass-helper: read stdin: {err}");
            return ExitCode::from(1);
        }
        prompt
    };
    if prompt.last() != Some(&b'\0') {
        prompt.push(b'\0');
    }

    if let Err(err) = stream.write_all(&prompt) {
        eprintln!("zed-askpass-helper: write socket: {err}");
        return ExitCode::from(1);
    }
    // Half-close the write side so the server knows the prompt is
    // complete and can start replying. Without this, the server's
    // `read_to_end` would block forever waiting for more bytes.
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("zed-askpass-helper: shutdown write: {err}");
        return ExitCode::from(1);
    }

    let mut response = Vec::new();
    if let Err(err) = stream.read_to_end(&mut response) {
        eprintln!("zed-askpass-helper: read socket: {err}");
        return ExitCode::from(1);
    }

    if let Err(err) = io::stdout().write_all(&response) {
        eprintln!("zed-askpass-helper: write stdout: {err}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

/// Git's Android subprocess path can discard non-Git environment variables
/// before invoking GIT_ASKPASS. GitHub credentials already have a stable,
/// Keystore-backed socket used by terminals and gh, so use that same bridge
/// rather than failing private HTTPS clones when ZED_ASKPASS_SOCKET is absent.
fn github_askpass_fallback() -> ExitCode {
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    let normalized_prompt = prompt.to_ascii_lowercase();
    if !normalized_prompt.contains("github.com") {
        eprintln!(
            "zed-askpass-helper: missing --askpass=<socket> and ZED_ASKPASS_SOCKET for non-GitHub prompt"
        );
        return ExitCode::from(2);
    }

    let Some(socket) = github_credential_socket() else {
        eprintln!("zed-askpass-helper: cannot locate GitHub credential socket");
        return ExitCode::from(2);
    };
    let mut stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!(
                "zed-askpass-helper: connect GitHub credential socket {}: {err}",
                socket.display()
            );
            return ExitCode::from(1);
        }
    };
    if let Err(err) = stream.write_all(b"protocol=https\nhost=github.com\n\n") {
        eprintln!("zed-askpass-helper: write GitHub credential request: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("zed-askpass-helper: finish GitHub credential request: {err}");
        return ExitCode::from(1);
    }

    let mut response = String::new();
    if let Err(err) = stream.read_to_string(&mut response) {
        eprintln!("zed-askpass-helper: read GitHub credential response: {err}");
        return ExitCode::from(1);
    }
    let requested_field = if normalized_prompt.contains("username") {
        "username"
    } else if normalized_prompt.contains("password") {
        "password"
    } else {
        eprintln!("zed-askpass-helper: unsupported GitHub prompt");
        return ExitCode::from(2);
    };
    let value = response.lines().find_map(|line| {
        line.strip_prefix(requested_field)
            .and_then(|value| value.strip_prefix('='))
    });
    let Some(value) = value else {
        eprintln!("zed-askpass-helper: no stored GitHub {requested_field}");
        return ExitCode::from(1);
    };
    if let Err(err) = writeln!(io::stdout(), "{value}") {
        eprintln!("zed-askpass-helper: write GitHub credential response: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn github_credential_socket() -> Option<PathBuf> {
    // <prefix>/.zed/bin/zed-askpass-helper -> <prefix>/tmp/github-credential.sock
    let executable = env::current_exe().ok()?;
    let prefix = executable.parent()?.parent()?.parent()?;
    Some(prefix.join("tmp/github-credential.sock"))
}

fn git_credential(socket: &str) -> ExitCode {
    let operation =
        env::args().find(|argument| matches!(argument.as_str(), "get" | "store" | "erase"));
    if operation.as_deref() != Some("get") {
        return ExitCode::SUCCESS;
    }

    let mut request = Vec::new();
    if let Err(err) = io::stdin().read_to_end(&mut request) {
        eprintln!("zed-askpass-helper: read credential request: {err}");
        return ExitCode::from(1);
    }

    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("zed-askpass-helper: connect credential socket {socket}: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = stream.write_all(&request) {
        eprintln!("zed-askpass-helper: write credential request: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("zed-askpass-helper: finish credential request: {err}");
        return ExitCode::from(1);
    }

    let mut response = Vec::new();
    if let Err(err) = stream.read_to_end(&mut response) {
        eprintln!("zed-askpass-helper: read credential response: {err}");
        return ExitCode::from(1);
    }
    if let Err(err) = io::stdout().write_all(&response) {
        eprintln!("zed-askpass-helper: write credential response: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
