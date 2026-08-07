# Zdroid-B technology stack and engineering techniques

This document describes the technology added or materially changed by the
Zdroid-B branch on top of the upstream Zed Android port. It reflects the branch
through `v1.0.0` and commit `1c47b7095d`.

## System architecture

```text
Android application (Kotlin, GameActivity, AndroidX)
  | JNI callbacks, lifecycle, permissions, services, SAF, IME
  v
Zed application process (Rust 2024, GPUI)
  |-- editor, workspace, Git UI, terminal, settings, agents
  |-- wgpu -> Vulkan -> Android Surface
  |-- ACP -> Codex / Claude / other CLI agents
  |-- OpenAI-compatible HTTP -> local llama-server
  v
Selected runtime adapter
  |-- Zdroid Bootstrap (Termux-derived bionic userland)
  |-- Kali chroot through zd-spawnd (root)
  `-- External Termux adapter (interface present, IPC bridge incomplete)
```

The Rust `cdylib` is loaded into the Android app process. Kotlin owns Android
framework integration while Rust owns the editor, GPUI views, model provider,
Git workflows, terminal sessions, and most application state.

## Core stack

| Area | Technology | How Zdroid-B uses it |
| --- | --- | --- |
| Main application | Rust 2024, Cargo workspace | Builds the Zed editor and Android platform layer as an `arm64-v8a` `cdylib`. |
| UI | GPUI and Zed `ui` components | Native rendered editor, responsive workspace, searchable pickers, modal flows, Agent Panel, settings, and Git UI. |
| Graphics | wgpu, Vulkan 1.1, Android native surfaces | GPU-composited editor with Android lifecycle and DeX/freeform-window handling. |
| Android shell | Kotlin, AndroidX, GameActivity, Java 17 | Activity lifecycle, permissions, IME, SAF, notification service, Keystore, browser intents, and selection handles. |
| Native bridge | JNI and `android-activity` | Bidirectional events for input, storage pickers, task status, credentials, DNS, URLs, and window state. |
| Async work | GPUI tasks, `smol`, Tokio, futures | Keeps network, ACP, downloads, runtime setup, and model-server readiness work off the render path. |
| Networking | Zed reqwest fork, rustls native roots, Android DNS resolver | HTTPS for GitHub, authentication, model downloads, and local OpenAI-compatible requests. |
| Persistence | Zed DB/sqlez, settings JSON, app-private files | Workspaces, ACP thread metadata, conversation history, unfinished drafts, runtime configuration, and model state. |
| Android build | Gradle 8, Android Gradle Plugin, NDK r27, cargo-ndk | Reproducible Rust/Kotlin release build, native helper staging, runtime-contract checks, and APK signing. |
| Supported target | Android API 26+, `arm64-v8a` | `targetSdk 28` intentionally preserves private app-data execution for the bundled runtime. |

## Runtime and terminal techniques

### Runtime-adapter boundary

Every Zed subprocess is routed through `zdroid_runtime` instead of assuming a
desktop POSIX host. The selected adapter supplies `HOME`, `PATH`, executable
paths, and spawn behavior:

- **Zdroid Bootstrap:** a Termux-derived bionic userland under
  `/data/data/com.zdroid/files/usr`.
- **Kali chroot:** forwards process creation to the privileged `zd-spawnd`
  daemon and a glibc root filesystem.
- **External Termux:** models the intent-based adapter contract; the final IPC
  dispatch is not yet implemented.

The application ID remains `com.zdroid` because the Bootstrap binaries,
RUNPATHs, and shebangs are package-path-sensitive. Gradle's
`verifyZdroidRuntimeContract` scans staged native artifacts for stale package
IDs and JNI signatures before packaging.

### Bootstrap resilience

Bootstrap installation is a visible task rather than a hidden first-run side
effect. It uses version sentinels, staging paths, integrity checks, package
repair, and a compatible package snapshot so an interrupted install does not
leave `dpkg` half-configured. Long-running terminal actions reuse the active
terminal instead of opening a new panel for every command.

### Terminal input

Zdroid-B implements a Termux-style two-row extra-key surface, terminal text
selection, sticky modifiers, arrow/navigation keys, viewport resizing above
the soft keyboard, and separate terminal/editor font scaling. Modifier state
is cleared when focus leaves the terminal to prevent `Ctrl` or `Alt` leaking
into editor and Agent Panel input.

## Android UI and input techniques

- **Responsive GPUI modals:** blocking flows use a common dimmed modal layer,
  explicit close/back actions, bounded dimensions, and independently scrollable
  content. They remain phone-friendly and become centered floating dialogs in
  DeX.
- **Focus-aware IME policy:** editor, terminal, Git commit input, dialogs, and
  Agent Panel prompts each control whether tapping should show the keyboard.
  Dropdown controls suppress IME activation.
- **Native selection overlay:** GPUI exposes selection geometry over JNI;
  Kotlin renders Android-style draggable handles and cut/copy/paste actions.
- **Touch gesture state machine:** differentiates tap, long press, drag, scroll,
  two-finger actions, and pinch zoom without emitting conflicting mouse events.
- **Desktop pointer behavior:** mouse-wheel translation, corrected window
  coordinates, and pointer-capture release allow normal DeX resize/move and
  movement outside the app.
- **SAF import boundary:** Android's Storage Access Framework selects external
  trees, while executable projects are imported into the app-private exec
  filesystem. `ZedDocumentsProvider` exposes the private home back to Android.

## Git and GitHub techniques

- GitHub uses the OAuth device flow with polling and a searchable account
  repository picker; users can also clone by URL.
- Clone UI separates repository selection, destination selection, and explicit
  Clone/Cancel actions so scrolling cannot accidentally select a repository.
- Git credentials are bridged between Zed's Git UI and terminal Git helpers.
  Secrets are encrypted with AES-256-GCM using a non-exportable Android
  Keystore key; the repository URL is authenticated additional data.
- The askpass relay is compiled as a static ARM64 helper so it can run from the
  chroot without resolving Android's dynamic linker.
- Git changes add mobile commit input, author identity setup, per-file and
  multi-file rollback, staging, stash, commit, push, diff, and history flows.

## Agent and authentication techniques

Zdroid-B uses the **Agent Client Protocol (ACP)** rather than translating CLI
output into a custom chat format. Android-specific launchers discover and run
the user's installed Codex and Claude CLIs inside the selected runtime. This
lets users authenticate with their existing CLI subscriptions instead of
embedding API keys in the APK.

Important compatibility work includes:

- managed Codex and Claude ACP launchers with registry fallback;
- browser URL forwarding through `OpenUrlReceiver` when a subprocess cannot
  call an Activity directly;
- Android DNS resolution for reqwest plus generated resolver data for native
  CLI binaries;
- foreground dispatch of ACP requests so GPUI state is only touched on its
  owning thread;
- local ACP thread metadata, history restoration, draft persistence, and agent
  model/configuration refresh;
- Agent Panel keyboard rules where Enter inserts a newline and the send button
  explicitly submits the prompt.

Other ACP-compatible CLIs can be registered through the same launcher and
handshake boundary without changing the conversation UI.

## Local LLM stack

The **Local LLM** provider is registered before Zed's cloud provider in the
language-model registry. It reuses Zed's OpenAI-compatible streaming request
path and talks to a loopback `llama-server`; it is not a separate ACP agent.

### Model lifecycle

1. Select a built-in model through a fuzzy-searchable dropdown, import a local
   GGUF file, or provide a trusted HTTPS GGUF URL.
2. Stream the model into a staging file while reporting task and notification
   progress.
3. Validate the `GGUF` magic header and calculate SHA-256 before activation.
4. Install `llama.cpp` and its Vulkan backend through the selected runtime when
   needed.
5. Start `llama-server` on loopback and poll its readiness endpoint.
6. Prefer Vulkan offload; if the server exits during startup, retry on CPU.
7. Persist runtime controls for CPU threads, context size, batch size, output
   tokens, and Vulkan use.

Downloaded models are presented first and removed from the browse list. Model
deletion requires a critical warning modal because it permanently removes the
large GGUF file. The built-in catalog is intentionally limited to Qwen,
DeepSeek, Gemma, and Llama variants no larger than 7B for mobile hardware.

## Background execution

`AgentForegroundService` represents the entire editor as one Android foreground
session rather than creating a permanent notification for each command. Rust
reports task start/finish events over JNI; the service updates a running-task
count and emits a separate completion notification when a batch finishes.

The notification exposes **Exit** and **WakeLock on/off** actions. Wake lock
mode holds both a partial CPU wake lock and a high-performance Wi-Fi lock for
long downloads, package upgrades, agent runs, and local inference. The mode is
user-controlled because it increases battery use and heat.

## Security and release techniques

- Credentials are never bundled in the APK and are scoped to each Android
  installation.
- Git secrets use Android Keystore-backed authenticated encryption.
- Custom model downloads require HTTPS and GGUF validation; the UI warns that
  native model parsers still require trusted sources.
- Runtime actions and agents can execute repository code, so untrusted projects
  should not be opened or trusted.
- Release APKs use one persistent RSA-4096 signing identity and APK Signature
  Scheme v2, allowing future in-place upgrades.
- `targetSdk 28` is an explicit compatibility and security trade-off required
  for executing Bootstrap binaries from app-private storage; this build is not
  designed for Play Store policy compliance.

## Branch commit map

The following commits form the Zdroid-B feature line after the original 0.3.2
port. Later commits refine or replace parts of earlier implementations.

| Commit | Main contribution |
| --- | --- |
| `43080f9cc1` | Initial Android agent, GitHub, credential, responsive UI, and workflow integration. |
| `3bc0062076` | Shared GitHub credentials for terminal Git and static askpass relay. |
| `8ced312aaf` | Agent layout, IME, terminal input, and Android window-state fixes. |
| `5a9ee617bc` | Compatible bundled Codex ACP launcher. |
| `78afb8142a` | Codex DNS compatibility and dropdown IME suppression. |
| `19fec6391e` | Workspace restoration and refined mobile input routing. |
| `f8be47e2b3` | Searchable GitHub repository clone picker. |
| `1ff0905582` | Native Android text-selection handles and actions. |
| `2f7ad246bd` | Touch-safe, responsive clone workflow. |
| `8481ec7d0a` | Agent keyboard and clone-action focus fixes. |
| `fcfad6c2ea` | Additional Android ACP agent registration. |
| `8f895729ca` | Scrollable GitHub repository results. |
| `928fc37114` | Agent workflow persistence and editor pinch zoom. |
| `8ccce47325` | Mobile history restoration and Claude model refresh. |
| `abdc7c33d0` | Android foreground execution for agent tasks. |
| `7146f37425` | Terminal selection, runtime UX, and release-signing helper. |
| `29409fc44e` | Viewport resizing above the soft keyboard. |
| `3020b0b182` | Extra-window and settings scrolling fixes. |
| `1ee0b5cc7c` | Package/runtime path migration and first-run mobile setup. |
| `d2c71a4c72` | Responsive Welcome and clone layout refinements. |
| `52453c32ea` | Runtime contracts, Bootstrap hardening, cursor, SAF, and mobile UX. |
| `234ebf0faf` | Runtime repair, CLI launchers, DNS, command actions, and setup hardening. |
| `1c47b7095d` | Version 1.0 local LLM provider, final responsive UI, task session, and release documentation. |

## Primary implementation locations

| Concern | Source |
| --- | --- |
| Android application integration | `crates/gpui_android/examples/zed_android/` |
| GPUI Android platform | `crates/gpui_android/src/` |
| Runtime adapters | `crates/zdroid_runtime/src/` |
| Local LLM provider | `crates/language_models/src/provider/open_ai_compatible.rs` |
| ACP transport and sessions | `crates/agent_servers/src/acp.rs` |
| Agent history and mobile UI | `crates/agent_ui/src/` |
| GitHub authentication and clone UI | `crates/git_ui/src/github_auth.rs`, `crates/git_ui/src/git_ui.rs` |
| Foreground service and secure credentials | `android/app/src/main/kotlin/com/zdroid/` |
| Responsive workspace/modals | `crates/workspace/src/`, `crates/settings_ui/src/` |

