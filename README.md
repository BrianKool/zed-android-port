<p align="center">
  <img src="crates/gpui_android/examples/zed_android/docs/screenshots/zdroid-logo.png" width="120" alt="Zdroid logo" />
</p>

<h1 align="center">Zdroid-B</h1>

<p align="center"><sub><em>Zed on Android.</em></sub></p>

<p align="center">
  Zed on Android, shaped for phones, tablets, and Samsung DeX.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/release-1.0.1-2ea44f" alt="Release 1.0.1" />
  <img src="https://img.shields.io/badge/platform-Android-3DDC84?logo=android" alt="Android" />
  <a href="https://github.com/BrianKool/zed-android-port/releases/latest"><img src="https://img.shields.io/github/downloads/BrianKool/zed-android-port/total?label=downloads" alt="Total downloads" /></a>
</p>

Zdroid-B is BrianKool's independent fork of the [Zed Android port](https://github.com/Dylanmurzello/zed-android-port), not affiliated with Zed Industries. It keeps Zed's native Rust editor and GPUI rendering while adding a responsive mobile interface, an integrated Linux-style terminal environment, subscription-based AI agents, GitHub workflows, and on-device GGUF models. It targets arm64 devices running Android 9 or newer; phone layouts down to 360 logical pixels and Samsung DeX are first-class use cases.

## Zdroid-B 1.0 highlights

- **Native Zed workspace:** editor, project tree, search, Git changes, terminal, extensions, keymaps, command palette, and persisted workspaces.
- **AI agents without separate API billing:** install and sign in to Codex CLI or Claude Code in the Zdroid-B terminal, then use them through ACP in the Agent Panel. Conversations and unfinished drafts persist locally.
- **Local LLM provider:** download, verify, start, stop, search, import, and delete GGUF models directly from Settings. The built-in mobile catalog includes Qwen Coder, DeepSeek, Gemma, and Llama models up to 7B.
- **Accelerated local inference:** configurable CPU threads, context, batch size, and output limit, with Vulkan GPU offload and automatic CPU fallback through `llama.cpp`.
- **GitHub integration:** device-flow sign-in, account-aware repository browser, searchable clone flow, shared Git credentials for the terminal and Git panel, commit identity setup, staging, rollback, stash, commit, push, and history.
- **Mobile and DeX input:** Android text selection, soft-keyboard focus rules, Termux-style terminal keys, independent editor and terminal font sizes, mouse wheel support, corrected pointer coordinates, and pointer release outside the app.
- **Responsive modal workflow:** Settings, runtime selection, clone, trust, GitHub account, and other blocking flows appear as dismissible in-app dialogs that fit phone and DeX layouts.
- **Background execution:** one foreground session notification keeps terminal and agent tasks alive, supports wake lock control, reports running task count, and notifies when all tasks finish.
- **Safer bootstrap setup:** first-run runtime selection, download progress, package repair, package snapshots, and command actions for installing or signing in to popular agent CLIs.

For the complete implementation architecture, dependency stack, Android/Rust
boundaries, security decisions, and a commit-by-commit map of the Zdroid-B
feature branch, see [**Zdroid-B technology stack and engineering techniques**](docs/ZDROID_B_TECH_STACK.md).

---

<p align="center">
  <img src="https://github.com/user-attachments/assets/68f763ba-e051-4217-8779-7bb9327f4a13" alt="Zdroid demo" width="100%" />
</p>

Vulkan via wgpu. AChoreographer-driven vsync, no JNI hop per frame. Opt-in 120Hz with Mailbox present mode. Glyph fallback into `/system/fonts` so Powerline arrows and CJK render without bundling fonts. The `Editor`, `Workspace`, `Project`, `MultiWorkspace`, `Search`, `GitPanel`, `GitGraph`, `Extensions`, and `Terminal` crates run unchanged. The Rust `.so` is the app process. gpui composites every pixel (yes, you read that right) straight into the Adreno Vulkan driver. Multi-Activity OS-chromed extra windows so DeX freeform renders Settings and secondary editors with real chrome.

Termux userland rebuilt under `com.zdroid` (applicationId byte-length pinned to 12 because prebuilt RUNPATHs in the .debs don't stretch). Musl loader hex-patched at runtime so Bun-compiled binaries like claude-code and codex resolve `/etc/resolv.conf` to a JNI-populated `/sdcard/.zed/r`. Optional Magisk-flashable `zd-spawnd` daemon with SCM_RIGHTS stdio relay for the chroot runtime. SurfaceControl-composited hardware cursor sprite on a sibling overlay, separate from the wgpu frame. Pointer-capture trackpad that consumes historical motion samples so finger drags don't lose 80% of their travel to event batching. SAF DocumentsProvider exposing `~/` as a system volume. Native Android trust via `rustls-platform-verifier`. In-app updater pulling signed APKs from GitHub Releases. Everything else is upstream. Deep-dives for the platform layer live in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/).

---

## <img src="https://api.iconify.design/lucide:download.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Install

Download [`Zdroid-B-1.0.1.apk`](https://github.com/BrianKool/zed-android-port/releases/latest) from the latest release and open it in your file manager. Android prompts for permission to install unknown apps the first time. Later Zdroid-B releases can upgrade in place because they use the same release signing certificate.

> [!NOTE]
> Android may show a "built for an older version of Android" warning before you tap Install. Proceed anyway. `targetSdk` is pinned at 28 on purpose: the bundled Termux userland depends on the `untrusted_app_27` SELinux domain, which permits `execve` on app-private files. Bumping `targetSdk` to 29+ lands the process in a stricter domain that denies exec, and the entire runtime stops working. See [`docs/workarounds/targetsdk-28-execve.md`](crates/gpui_android/docs/workarounds/targetsdk-28-execve.md) for the receipts.

### First launch

1. **Permissions.** Zdroid-B requests storage access first and notification permission second. Storage access is needed for projects outside the app sandbox; notifications are needed for the foreground background-execution session.
2. **Runtime adapter.** The in-app runtime dialog then asks where terminal commands, language servers, Git, and agent CLIs should run. Choose **Zdroid Bootstrap (recommended, AI agents needed)** for the supported no-root setup.
3. **Bootstrap setup.** The app downloads, verifies, extracts, and repairs its Termux-derived userland with visible progress. Keep Zdroid-B open for the initial setup; later launches reuse the installed runtime.

### Setting up your shell environment (Bootstrap)

Open the integrated terminal. The Welcome page's Agent information panel can run each command in the existing terminal for you. To update manually, run the commands separately:

```sh
pkg update -y
pkg upgrade -y
```

Install Node.js, Git, and the agent CLIs you want:

```sh
pkg install -y nodejs-lts git
npm install -g @openai/codex
npm install -g @anthropic-ai/claude-code
```

Run `codex` or `claude` once in the terminal and complete the browser or interactive subscription login. Credentials stay on that device and are not included in the APK. Toolchains and additional package recipes live in [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap).

### Setting up Kali chroot

Prerequisites: a rooted device with Magisk installed.

1. Drop a Kali NetHunter aarch64 rootfs at `/data/local/nhsystem/kali-arm64`. NetHunter's installer is the standard path; any aarch64 Debian-derived rootfs at that location works.
2. Flash the [`zd-spawnd`](https://github.com/Dylanmurzello/zdroid-spawnd/releases) Magisk module. Reboot.
3. Open Zdroid and pick **Kali chroot** in the runtime picker.

Every subprocess the editor spawns (`bash`, `git`, LSPs, terminal shells) goes over a Unix socket to the `zd-spawnd` daemon, which `fork`s, `chroot`s, drops privileges, and `execve`s on your behalf. Sub-millisecond spawn versus ~200 ms for `su`-mediated alternatives. All the bionic-vs-glibc gotchas (`/usr/bin/env`, `/tmp`, `dlopen libfoo.so`) disappear because subprocesses run inside a real distro.

### Setting up External Termux

> [!IMPORTANT]
> External Termux is partially wired today. The runtime-picker entry exists and persistence works, but the JNI Intent bridge that actually dispatches subprocesses to Termux's `RUN_COMMAND` service hasn't landed yet ([`crates/zdroid_runtime/src/adapters/external_termux.rs`](crates/zdroid_runtime/src/adapters/external_termux.rs) is stubbed). Picking this adapter today writes the selection, but subprocess calls don't reach Termux. Use Bootstrap or Kali chroot for actual work in the meantime.

Setup, once the bridge lands:

1. Install Termux from [GitHub releases](https://github.com/termux/termux-app/releases).
2. The first time Zdroid attempts an external spawn, Android prompts you to grant `com.termux.permission.RUN_COMMAND` to Zdroid. Allow it.
3. Open Zdroid and pick **External Termux** in the runtime picker.

After that, every subprocess routes into your existing Termux setup via `com.termux.app.RunCommandService`. Your `~/`, your packages, your shell history are what subprocesses see; Zdroid stays a thin spawner.

### Working with projects

Two storage realms underneath, with different exec rules.

`/data/data/com.zdroid/files/` (surfaced as `~/`) is **exec-mounted**. cargo, go, node, anything you build can `execve` and run. This is where projects should live. `~/projects/<name>` is the default workspace root; `ZedDocumentsProvider` exposes `~/` to other Android apps via the SAF sidebar (look for **Zdroid-B** in any system file picker).

`/storage/emulated/0/` (a.k.a. `/sdcard/`) is **FUSE-mounted with `noexec`**. Read, edit, and save all work; the kernel refuses to execute binaries written here. `cargo run` against a binary under `/sdcard/...` returns `EACCES` and there's no remount workaround (see [`docs/workarounds/android-noexec-mount.md`](crates/gpui_android/docs/workarounds/android-noexec-mount.md) for why).

Three workflows that work with this constraint:

- **Project root under `~/projects/`** is the happy path. `git clone`, `cargo new`, builds, debugs, terminal subprocesses all run. Browse there from any Android app via the **Zdroid → projects** SAF sidebar entry.
- **Open a folder anywhere on `/sdcard/`** is fine if you're only reading or editing. The title bar shows a yellow **Builds won't run · Move** chip; one tap pops a confirm dialog that copies the project into `~/projects/<basename>` and reopens it from the exec side.
- **`~/storage/{shared,downloads,dcim,documents,…}`** are curated symlinks into `/sdcard`. Use them for "open and edit a single file" workflows where you don't want to copy a whole tree.

---

## <img src="https://api.iconify.design/lucide:hand.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Gestures & input

Each surface toggles in **Settings → Android Input** (or on the first-run onboarding card).

### Touch (default)
- **Tap** to position the cursor
- **Tap and drag** to scroll
- **Two-finger tap** for right-click
- **Long-press** to select a word
- **Long-press and drag** to extend the selection
- **Scrollbar tap and drag** snaps the thumb to your finger and follows

### Virtual trackpad mode
Toggle the crosshair icon in the tab bar to turn touch into a pointer. Direct touch is disabled while it's on.
- **One-finger drag** moves the cursor
- **Tap** for left click, **two-finger tap** for right click
- **Hold then drag** for text selection
- **Two-finger drag** to scroll the content

### Hardware mouse / trackpad
Plug in or pair a mouse, trackpad, or Book Cover Keyboard and it just works.
- **Scroll wheel** and **two-finger trackpad scroll** to scroll
- **Right-click** anywhere for context menus
- The cursor hides when you switch to touch or typing and comes back on the next pointer event

> Working on Samsung phones, tablets, and DeX in the configurations tested for Zdroid-B. If your mouse pairs but does not move the cursor, [open an issue](https://github.com/BrianKool/zed-android-port/issues/new) with `adb shell dumpsys input` output so the device-specific path can be diagnosed.

### Soft keyboard
Auto-opens on tap into the editor or terminal. The pane tab bar has a keyboard toggle if you want it off.
- **Programming keys row** above the keyboard with `Esc`, `Tab`, `Ctrl`, `Alt`, and arrows. `Ctrl` and `Alt` are sticky: tap once for the next key, double-tap to lock.
- **`Ctrl` + letter** combos work in the terminal: `Ctrl+C` sends `^C`, just like a hardware keyboard.

### Dialogs and DeX
Blocking workflows use centered in-app modal dialogs with a dimmed backdrop, explicit close/back controls, and independent scrolling. They remain modal on phones and become floating dialogs in DeX instead of creating stale Android Recents entries.

---

<a id="userland"></a>
## <img src="https://api.iconify.design/lucide:server.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Userland

The editor is bionic-linked and runs as the Android app process. Every subprocess it spawns (`bash`, `apt`, language servers, formatters, terminal shells, `git`, `ssh`) routes through whichever runtime adapter the user picked. Three adapters ship; they version independently of the editor APK.

| Adapter | What it is | Where it comes from |
|---|---|---|
| **Bootstrap** _(no root)_ | Termux userland rebuilt under `com.zdroid`: apt/dpkg/bash with our package name baked into RUNPATHs and shebangs. Pure bionic, no glibc; same trade-offs as any Termux install. `apt` and `pkg install` work for everything Termux ships. | Downloaded from [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap) after you pick Bootstrap in the runtime picker. |
| **Kali chroot** _(needs Magisk)_ | Real glibc Linux. Every spawn goes over a Unix socket to `zd-spawnd` (a small privileged daemon) which does `fork` + `chroot` + `setuid` + `execve` on the editor's behalf. ~5 ms per spawn vs ~200 ms for `su`-mediated. All the bionic gotchas (`/usr/bin/env`, `/tmp`, `dlopen libfoo.so`) disappear because subprocesses run inside a real distro. | Flash the Magisk module from [`Dylanmurzello/zdroid-spawnd`](https://github.com/Dylanmurzello/zdroid-spawnd), plus drop a Kali NetHunter aarch64 rootfs at `/data/local/nhsystem/kali-arm64`. |
| **External Termux** _(if you already use Termux)_ | Talks to your existing Termux app via `com.termux.permission.RUN_COMMAND` intents. Lighter footprint; your existing userland stays untouched. JNI Intent bridge in progress (adapter at [`crates/zdroid_runtime/src/adapters/external_termux.rs`](crates/zdroid_runtime/src/adapters/external_termux.rs) is stubbed). | Install Termux from [GitHub releases](https://github.com/termux/termux-app/releases); grant `RUN_COMMAND` to Zdroid. |

Switching is one tap (Settings → Android Runtime). Selection persists in `$PREFIX/etc/zd-runtime.toml`. **Restart Zdroid after switching adapters.** The editor caches environment state (PATH, HOME, library search paths, spawn-router config) from whichever adapter was active at boot; without a restart, subprocesses spawned post-switch can land with stale env and fail in cryptic ways (LSPs not found, `git` claiming HOME doesn't exist, `pkg install` writing to the wrong rootfs, etc.).

### When to pick which

- **No root, just want it to work:** Bootstrap. apt, npm, `go install`, rust-analyzer all work. Rough edges: precompiled Bun CLIs (claude-code, codex) rely on a runtime hex-patch of `/etc/resolv.conf` to a `/sdcard/.zed/r` file that JNI populates each boot from `ConnectivityManager.getActiveDnsServers()`. Full writeup in [`zdroid-bootstrap/docs/hex-patch-resolv-conf.md`](https://github.com/Dylanmurzello/zdroid-bootstrap/blob/main/docs/hex-patch-resolv-conf.md). Some glibc-only extension binaries don't run.
- **Have Magisk, want real Linux:** Kali chroot. Everything you'd expect on Debian/Kali works as-is, no shimming needed. The chroot is shared with whatever else uses that NetHunter rootfs.
- **Already on Termux:** External adapter once the bridge lands. Your `~/`, your packages, your shell history; Zdroid just spawns subprocesses there.

---

## <img src="https://api.iconify.design/lucide:layers.svg?color=%23999999&height=22" valign="middle" /> &nbsp;What works

<p align="center">
  <table>
    <tr>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/ssh_workspace.jpg" alt="Remote SSH workspace" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/git_graph.jpg" alt="Git graph" /></td>
      <td><img src="crates/gpui_android/examples/zed_android/docs/screenshots/settings.jpg" alt="Settings" /></td>
    </tr>
  </table>
</p>

- **Editor.** Vulkan rendering, multi-pane workspace, vim mode, syntax highlighting, project panel, fuzzy file finder, command palette, buffer + project search.
- **Git and GitHub.** Device-flow account login, repository browser and clone flow, credential sharing, staging, rollback, stash, commit, push, diff, and commit history.
- **Agents.** Codex and Claude through ACP using each CLI's own subscription login, local conversation history, draft recovery, model controls, and agent task notifications.
- **Local models.** Searchable and removable Qwen, DeepSeek, Gemma, and Llama GGUF catalog, custom HTTPS/file import, integrity checks, progress, Vulkan acceleration, and CPU fallback.
- **LSPs.** rust-analyzer baked in. gopls, ts-server, pyright, jdtls install in one `pkg`/`npm`/`go install`.
- **Extensions.** Browse, install, manage. Themes, language configs, grammars, slash commands.
- **Remote SSH.** Server-picker pill in the title bar, persisted server list, native askpass.
- **Responsive dialogs.** Modal workflows remain inside the workspace, fit narrow phone screens, and use centered floating surfaces in DeX.
- **Background sessions.** A foreground service and optional wake lock keep terminal and agent work active while the app is backgrounded.
- **Edge-to-edge** rendering with content under the display cutout.
- **App menu bar** with nested submenus (Settings, Keymap, Themes, Extensions).
- **Theme follow** for system light/dark.
- **`ZedDocumentsProvider`** exposes the project root as a SAF volume, so other Android apps can browse Zed's worktrees.

---

## <img src="https://api.iconify.design/lucide:hammer.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Build from source

You'll need:

- Rust toolchain with `aarch64-linux-android` (`rustup target add aarch64-linux-android`)
- [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) (`cargo install cargo-ndk`)
- Android NDK r27 (`sdkmanager "ndk;27.0.12077973"`)
- Gradle 8+, `adb` on `$PATH`
- A device with USB debugging on

```sh
cd crates/gpui_android/examples/zed_android

ANDROID_NDK_HOME=/path/to/ndk/27.0.12077973 \
  cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build

cd android
gradle assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.zdroid/com.zdroid.MainActivity

adb logcat -d | grep -E "zed_android|RustPanic|FATAL"
```

First build is around 10 minutes. Incremental Rust rebuilds are 20 seconds, Gradle re-pack a few seconds.

---

## <img src="https://api.iconify.design/lucide:tablet-smartphone.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Tested on

Zdroid-B 1.1.1 targets Samsung phones at 360 logical pixels and Samsung DeX, including touch input, a hardware mouse and keyboard, GitHub login, Codex, Claude, Managed Linux, and Vulkan-accelerated local LLM inference. It compiles for arm64 Android 9+ with Vulkan 1.1. Other GPU families may require device-specific Vulkan tuning.

Touch-only phone use, tablet keyboards, Bluetooth input, and DeX are supported. Local 7B inference needs several gigabytes of free RAM and can heat the device; smaller 1B-3B models are the practical default for longer mobile sessions.

---

## <img src="https://api.iconify.design/lucide:file-text.svg?color=%23999999&height=22" valign="middle" /> &nbsp;License

GPL-3.0-or-later, same as upstream Zed. The Bootstrap-adapter zip (distributed from [`Dylanmurzello/zdroid-bootstrap`](https://github.com/Dylanmurzello/zdroid-bootstrap), not bundled in the APK) contains Termux-rebuilt packages each under its own license (mostly BSD/MIT/Apache; gnupg/bash/coreutils are GPL). The Alpine-derived `ld-musl-aarch64.so.1` inside it is MIT. The `zd-spawnd` daemon ([`Dylanmurzello/zdroid-spawnd`](https://github.com/Dylanmurzello/zdroid-spawnd)) is GPL-3.0-or-later.

© Dylan Murzello, distributed under GPL-3.0-or-later. Zed itself is © Zed Industries.

---

## <img src="https://api.iconify.design/lucide:handshake.svg?color=%23999999&height=22" valign="middle" /> &nbsp;Acknowledgments

- [Zed Industries](https://zed.dev/) for [`gpui`](https://github.com/zed-industries/zed/tree/main/crates/gpui) being platform-agnostic enough that an Android port is plumbing rather than a rewrite.
- The [`wgpu`](https://github.com/gfx-rs/wgpu) and [`blade-graphics`](https://github.com/kvark/blade) maintainers for a Vulkan abstraction that just works on Adreno.
- [The Termux project](https://termux.dev/) for [a decade of Linux-on-Android](https://github.com/termux/termux-app). Most of our `apt install` machinery is their patches with the package name swapped.
- [Alpine Linux](https://alpinelinux.org/) for [musl libc](https://musl.libc.org/), which lets Bun-compiled musl binaries (claude-code, codex) execve cleanly on bionic.

---

## <img src="https://api.iconify.design/lucide:circle-help.svg?color=%23999999&height=22" valign="middle" /> &nbsp;So why this ?

Zed Industries' position on a mobile/tablet port: **not planned**.

- [#12039 IOS/Android Port](https://github.com/zed-industries/zed/issues/12039), open since May 2024.
- [#34633 start of termux build](https://github.com/zed-industries/zed/issues/34633), closed as "not planned" in Jul 2025.
- [#43207 gpui: On Android](https://github.com/zed-industries/zed/issues/43207), open in the GPUI Roadmap as "Wide Scope" since Nov 2025.

This repo is what those threads were asking for, built independently. The Termux build attempt failed because the upstream `wasmtime`/`cranelift` deps don't compile inside Termux. We sidestep that by building the APK on a desktop with `cargo-ndk` and running our own custom Termux userland in process. No fork of upstream-Zed-with-android-cfg is needed; the Editor, Workspace, Project, Search, GitGraph, Terminal, Extensions crates run unchanged. The work is at the platform boundary, documented in [`crates/gpui_android/docs/workarounds/`](crates/gpui_android/docs/workarounds/).
