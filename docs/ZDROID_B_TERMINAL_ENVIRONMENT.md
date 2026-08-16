# Zdroid-B Terminal Environment Guide

This guide explains how Zdroid-B turns an Android phone into a practical
development environment, how its terminal runtimes fit together, and what to do
when a tool needs repair.

## Mental Model

Zdroid-B runs inside one Android app sandbox:

```text
Zdroid-B app
  -> Android Bootstrap runtime
  -> Ubuntu Managed Linux runtime
  -> shared projects, credentials and background tasks
```

The goal is that users can treat Zdroid-B like one development tool, even though
internally it needs two cooperating runtimes.

## Runtime Layers

### Android Bootstrap

Bootstrap is the Android-native, Termux-flavored runtime. It uses Android's
Bionic libc and lives under:

```text
/data/data/com.zdroid/files/usr
/data/data/com.zdroid/files/home
```

Bootstrap is best for app-integrated tools:

- Node.js and npm used by Zdroid-B agent launchers.
- Codex CLI subscription login.
- Claude CLI subscription login.
- GitHub CLI credentials.
- Git, SSH and GitHub helper integration used by the Git panel.

### Ubuntu Managed Linux

Managed Linux is a PRoot Ubuntu userland. It gives Zdroid-B glibc compatibility
for Linux software and lives inside the app sandbox.

Ubuntu is best for normal development commands:

- `apt` packages.
- Python CLI tools.
- `pipx`, `venv`, build tools and native Python packages.
- Local servers and databases when ARM64 Linux builds are available.
- Tools that expect a glibc Linux filesystem.

## Command Bridge

Zdroid-B bridges the two runtimes so users do not need to constantly think about
which side owns a command.

From the terminal, these commands should route into Ubuntu when Full Linux is
installed:

```sh
python
python3
pip
pip3
uv
graphify
```

From Ubuntu, these commands can route back to Bootstrap credentials:

```sh
codex
claude
gh
```

This lets a user install a Python tool in Ubuntu while still using the same
Codex, Claude and GitHub account state that the Agent Panel uses.

## Check The Active Runtime

Open a terminal and run:

```sh
echo $ZDROID_RUNTIME
which python3
which pip
which codex
which claude
which gh
```

Expected Full Linux style output should mention Ubuntu/glibc for
`$ZDROID_RUNTIME`, and Python commands should not be the Android/Bionic Python
unless the user intentionally selected Standard mode.

## Python Packages

Ubuntu follows PEP 668, so system-wide `pip install <package>` may fail with:

```text
externally-managed-environment
```

That is expected. Use one of these patterns instead.

For global Python CLI tools:

```sh
pipx install graphifyy
```

For project dependencies:

```sh
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

If `pipx`, `venv`, `python3.12` or native builds fail, open:

```text
Settings -> Android Runtime -> Python CLI Tools -> Repair
```

The repair action reinstalls Ubuntu Python, `pipx`, `venv`, compiler packages
and the Zdroid command bridges.

## Codex, Claude And GitHub

Codex and Claude can be used through the Agent Panel without API keys when the
user signs in with their subscription account.

Useful terminal commands:

```sh
codex login
claude
gh auth login
```

If the Agent Panel says Codex or Claude is missing, open the Welcome screen's
agent setup info and run the install/login action from there.

If Git commit says:

```text
Author identity unknown
```

configure Git once:

```sh
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
```

GitHub credentials are stored on the device. They are not bundled into the APK.

## Local Servers

For web apps, start the server in the terminal and open it in the phone browser:

```sh
npm run dev -- --host 0.0.0.0
```

Then open:

```text
http://localhost:<port>
```

Long-running commands are tracked as Zdroid-B background tasks. When tasks
finish, Zdroid-B can notify the user.

## Repair Flow

Use Android Runtime repair when tools look installed but fail with missing files.

Open:

```text
Settings -> Android Runtime
```

Then choose the relevant repair card:

- Full Linux: repairs or reinstalls the Ubuntu userland.
- Python CLI Tools: repairs Python, pipx, venv, build tools and command bridges.
- Standard runtime: repairs the Android Bootstrap package state.

The Welcome screen's Terminal Tutorial also includes an "Open Android Runtime"
button that opens this repair area directly.

## Common Problems

### `externally-managed-environment`

Use `pipx` for CLI tools or a project `venv` for project packages.

### `python3.12 not found`

Open Android Runtime and repair Python CLI Tools.

### `tree-sitter-*` builds mention Android tags

The command is probably running through the Android Bootstrap Python instead of
Ubuntu. Open a new terminal, run `echo $ZDROID_RUNTIME`, and repair Python CLI
Tools if needed.

### `No such file or directory` or `exit status 127`

The tool may be installed in one runtime while an absolute path from the other
runtime was used. Prefer short command names such as:

```sh
git
node
npm
python3
codex
claude
gh
```

Avoid passing absolute Bootstrap paths into Ubuntu:

```text
/data/data/com.zdroid/files/usr/bin/<tool>
```

### `dubious ownership`

Press **Trust Directory** in the Git panel, or configure Git safe directories.

### DNS or network errors

Check Android network state, VPN, DNS and certificate warnings. Some database
drivers such as MongoDB Atlas require SRV DNS lookups and can expose DNS issues
earlier than normal web browsing.

## Safety Notes

- Credentials stay on the device and are not included in the APK.
- Do not install or run tools from untrusted repositories.
- Do not ask agents to install PRoot or rewrite the app runtime unless you know
  why it is necessary.
- If a Linux binary is incompatible, check whether it is Android/Bionic,
  Linux/glibc, Linux/musl or static ARM64 before forcing an install.

