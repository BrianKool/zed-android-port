# Zdroid-B Phone, Browser, Voice Architecture and Audit

This document is the source of truth for the Android Phone Use, Browser Use,
voice conversation, and Agent safety work introduced after Zdroid-B 1.1.5.
Read it together with `AGENTS.md` before changing these systems.

## Product Decisions

- Phone Use must work on the phone itself. A PC, ADB pairing, root, and
  Shizuku are not normal-user requirements.
- Android Accessibility is the primary interaction API. Screenshots and
  coordinate gestures are fallbacks, not the default Agent interface.
- Browser Use is not a second autonomous browser. It combines live Android
  Chrome control with an optional public-page reading layer.
- Signed-in state and consequential actions always use the live Android
  surface. Crawl output must never be treated as proof of live browser state.
- Voice, text input, Agent output, and TTS are independent channels sharing one
  persistent ACP conversation. Voice mode must not block the UI.
- Pausing voice releases expensive resources and listens only for an explicit
  resume phrase when on-device recognition is available. Ending voice must not
  delete or end the ACP conversation.
- Simple, high-confidence phone commands may use a deterministic local reflex
  path. Ambiguous or semantic work belongs to Codex or Claude.
- Direct ACP file tools are denied project-external access and Agent project
  commands are denied Bootstrap mutations by default. Relaxing either
  guardrail requires explicit typed confirmation. This is not an OS sandbox
  for arbitrary shell commands.

## Runtime Boundaries

The existing two-runtime rule remains unchanged:

```text
Android Bootstrap (Bionic)
  - App integration, Node, Codex/Claude launchers, credentials, MCP proxy

Ubuntu Managed Linux (glibc/PRoot)
  - Linux compatibility, apt, Python/native packages, project commands
```

Phone Use's Node proxy is a Bootstrap component. Do not copy its executable
path into Ubuntu or create a second Phone Use installation in Ubuntu.

## Phone and Browser Data Flow

```text
Codex / Claude ACP
        |
        v
zdroid-phone-use-mcp (Bootstrap Node proxy)
        |  Bearer token, loopback client
        v
Droid-MCP HTTP transport inside com.zdroid
        |
        +-- BrowserUseRouterTool
        +-- PhoneActionPlanTool
        +-- ObserveSemanticUiTool
        +-- PerformSemanticActionTool
        +-- selected Droid-MCP device/app/accessibility tools
        |
        v
ZdroidAccessibilityService -> Android/Chrome UI
```

The runtime starts only when the Accessibility service is enabled. The service
also starts it from `onServiceConnected`; MainActivity only starts it when the
permission is already enabled. This avoids an idle Ktor/Netty server for users
who do not use Phone Use without introducing a reconnect race. `onUnbind`
queues shutdown on the same single-thread executor as startup, so disabling
Accessibility also releases the server without a start/stop race.

The MCP token is 256 random bits, stored in app-private preferences and copied
to an owner-only app-private file. The Node proxy fails closed if the token is
missing and aborts HTTP calls after 30 seconds.

App-private does not mean project-shell-private: Bootstrap/Ubuntu subprocesses
ultimately run under Zdroid-B's Android UID and a malicious repository command
may be able to read app-owned bridge material, including this token. Keep Phone
Use disabled for untrusted repositories. Preventing same-UID shell access also
requires the future Agent-specific mount/process sandbox described below.

### Residual Transport Risk

Droid-MCP 0.10.1 exposes a port parameter but no host/bind-address parameter.
Its documented HTTP mode is intended for local-network clients. Zdroid-B
requires authentication and does not advertise mDNS, but the listener may bind
beyond loopback. Verify the actual socket on each supported Android build.

A strict loopback-only transport requires one of these deliberate projects:

1. vendor Droid-MCP and add a host parameter;
2. replace its HTTP transport with a Zdroid-owned loopback MCP transport; or
3. bridge Rust/Node to Droid-MCP's in-process API.

Do not silently vendor the library during an unrelated fix.

## Semantic UI IR

`SemanticUiTools.kt` converts `AccessibilityNodeInfo` into a compact,
deterministic representation. The pipeline is:

```text
Accessibility tree
 -> iterative traversal
 -> visibility/password filtering
 -> empty-container flattening
 -> role normalization
 -> label resolution
 -> duplicate merge
 -> modal prioritization
 -> region grouping
 -> compact JSON
```

Supported observation modes:

- `COMPACT`: balanced overview.
- `ACTIONABLE`: interactive elements only.
- `READABLE`: text-oriented page understanding.
- `DELTA`: elements added or changed since the immediately previous revision.

Each observation has a revision. Element IDs are valid only for that revision.
`semantic_key` is continuity metadata and must never be sent to the action
executor. Every state-changing action invalidates the previous snapshot and the
Agent must re-observe.

Coordinates are intentionally absent from the primary IR. Locators retain
descriptive metadata, not `AccessibilityNodeInfo` objects, preventing stale
node retention and memory leaks. Action lookup walks the current tree and
rejects stale revisions.

## Browser Routing

`route_browser_use` selects the cheapest safe path but performs no network or
UI action itself:

- phone navigation or simple actions: local plan, then semantic UI;
- live interaction, signed-in pages, verification, or consequential actions:
  live Android semantic UI;
- explicit public read-only URL with optional Crawl4AI installed: Crawl4AI
  context, then return to live Android UI before interaction.

Crawl4AI is optional and lazy. It is not bundled, auto-started, or required for
Phone Use. This avoids shipping a Python/browser runtime and keeps APK/RAM use
bounded. Localhost, `.local`, file URLs, and private IPv4 literals are rejected
by the route helper. Any future fetch implementation must also resolve DNS and
reject private/link-local results to prevent SSRF and DNS rebinding.

`execute_action_plan` is limited to 12 deterministic actions. It allows app
launch, bounded waits, navigation, scrolling, and short delays. It deliberately
does not batch text entry, element clicks, purchases, submissions, deletion, or
other consequential actions.

## Voice Architecture

```text
VoiceConversationService (foreground microphone service)
  +-- RealtimeVoiceCapture (AudioRecord + AEC + NS)
  +-- Android streaming SpeechRecognizer
  +-- VoiceOrchestrator
  |     +-- FloorManager
  |     +-- VoiceInterruptionArbiter
  |     +-- VoiceTurnManager
  +-- DynamicEndpointing
  +-- ACP event adapter
  +-- VoiceSpeechPolicy
  +-- VoiceSpeechScheduler
  +-- Android TTS / AudioTrack-owned engine
  +-- VoiceLatencyTelemetry and MobileVadBenchmark
```

Audio capture uses Android audio thread priority rather than Java maximum
priority. Microphone permission is checked immediately before constructing
`AudioRecord`, because the user may revoke it after the call UI opens.

Three independent state groups describe the session:

- audio: microphone and speaker;
- user: silent or speaking;
- agent: idle, thinking, responding, or tool use.

Floor state is user, agent, shared, or undecided. `TurnId + revision` rejects
late ACP events after interruption or a newer prompt.

### Interruption

Interruption is two-stage:

1. acoustic/VAD evidence pauses audible output quickly;
2. a meaningful transcript confirms cancellation of TTS and the current Agent
   response.

A cough, echo, filler, or very short low-confidence sound resumes queued speech
after the false-interruption timeout. Confirmed speech or typed input
invalidates the output epoch, clears speech queues, stops TTS, interrupts ACP,
and starts a new turn in the same conversation.

The scheduler tracks generated, queued, and spoken cursors. Thinking, tool
calls, tool output, and code are not spoken. Agent message sentences are
chunked and spoken as streaming text arrives.

### Lifecycle

The service owns recognizer, `AudioRecord`, AEC/NS, TTS, callbacks, workers,
audio focus, wake lock, and Wi-Fi lock. `onDestroy` releases all of them.

Wake-only pause stops continuous capture, releases audio focus and Zdroid
session locks, and prefers Android on-device recognition. Active voice holds
the foreground microphone service and session locks so it can survive app
backgrounding and screen lock.

The MainActivity ACP event queue coalesces streaming messages and is capped at
64 events to prevent unbounded growth if the UI thread is temporarily blocked.

## Danger Zone

Default policy:

```properties
allow_agent_outside_project=false
protect_bootstrap_runtime=true
```

ACP file read/write and terminal cwd requests enforce the active-project
boundary. Relative paths containing parent/root/prefix components are rejected.
With Bootstrap protection enabled, Android Agent commands are routed into
Ubuntu unless they are already in Managed Linux or already use `zd-exec`.

The project boundary does not isolate arbitrary shell commands. A command can
name an absolute path itself, so true project-only shell isolation requires a
separate Agent-specific PRoot/mount namespace. The UI deliberately calls the
current feature a direct-file-tool guardrail rather than promising a sandbox.

Policy files are atomically written and owner-only. Enabling a dangerous option
requires typing its exact warning phrase. Re-enabling protection is immediate.

## File Provider Security

`ZedDocumentsProvider` validates every lexical document ID against app HOME,
rejects unsafe create/rename names, and prevents rename/delete of HOME itself.
Lexical containment is intentional: canonical containment would break the
supported HOME links into app-managed project locations.

## Background Tasks and Locks

`ZdroidSessionLocks` owns one non-reference-counted wake lock and Wi-Fi lock,
guarded by a synchronized owner set. Multiple tasks do not create multiple
locks. The locks are released when the last owner exits.

The global Background Execution setting intentionally may hold these locks
while idle so untracked interactive terminal processes keep running. This is a
product tradeoff, not a leak. A future battery-saving mode could hold locks only
for tracked tasks/voice, but it must first cover arbitrary terminal jobs and
must not silently change the Termux-like behavior users rely on.

## Security and Compatibility Constraints

- `targetSdk=28` and legacy storage are intentional requirements of the
  executable Termux-derived Bootstrap. This is not a modern Play Store sandbox.
- Only trusted repositories should be opened with unrestricted Agent access.
- `allowBackup=false`; services and normal providers are not exported.
- Accessibility is exported only behind Android's
  `BIND_ACCESSIBILITY_SERVICE` permission.
- URL broadcasts are same-app only and accept only HTTP(S).
- Release credentials are never committed; Gradle reads ignored
  `signing.properties`.
- APK replacement must compare signing certificate SHA-256 values and use
  `adb install -r`. Never uninstall or run `pm clear` when preserving data.

## Audit Results (2026-08-28)

Fixed during the audit:

- added Semantic UI modes, revisions, delta output, modal focus, normalization,
  stable continuity keys, iterative traversal, and telemetry;
- added safe Browser routing and bounded deterministic action plans;
- removed the old Playwright Android implementation and obsolete MainActivity
  voice overlay/dialog code;
- made Phone Use lazy when Accessibility is disabled;
- added MCP proxy authentication failure and request timeout handling;
- hardened DocumentsProvider path/name/root operations;
- checked microphone permission at capture time;
- guarded API 29/30-only language and accessibility APIs for minSdk 28;
- used Android audio thread priority instead of Java maximum priority;
- bounded the pending voice event queue;
- preserved stale-event rejection and false-interruption recovery;
- compacted the live-call layout while the IME is visible so typed text and
  conversation output remain above the keyboard;
- reduced the ongoing call notification to the two state controls supported by
  Android `CallStyle`, and made the paused-state Resume action explicit and
  high contrast;
- updated Android-target `h2` to 0.4.16, `anyhow` to 1.0.103,
  `event-listener` to 5.4.2, and `memmap2` to 0.9.11 to pick up compatible
  security/soundness fixes without a broad upstream dependency migration.

`cargo audit` still reports advisories for packages present in the Android
lockfile. `h2 0.3`, `quick-xml 0.30`, `quinn-proto 0.11.14`,
`rustls-webpki 0.101`, and `cxx 1.0.194` are not in the
`aarch64-linux-android` normal dependency graph. The packages that are in the
graph and cannot be safely patch-updated in this release are:

- `rsa 0.9.10`: upstream has no fixed release. Android accepts OAEP-SHA256 and
  compiles out the historical PKCS#1 v1.5 decryption fallback associated with
  the timing advisory.
- `circular-buffer 1.2.0`: the advisory requires a panic during element `Drop`
  followed by caught unwinding. Zdroid uses it for debugger output events and
  does not rely on that pattern; the fix requires a 2.x migration.
- `lru 0.16.4`: the advisory requires a panicking key destructor during
  `pop()` followed by caught unwinding. The Agent UI cache uses `Arc<str>` keys;
  the fix requires a 0.18 migration.
- `cgmath 0.18.0`: the affected API is `swap_columns` with identical indices;
  it is a transitive SVG dependency and Zdroid does not call that API. Upstream
  has no fixed release.

These are inherited upstream constraints rather than blockers introduced by
Phone Use or Voice. Revisit them as isolated dependency migrations with Android
rendering, debugger, networking, and Agent UI regression coverage.

Verification required before installation:

```text
Gradle Kotlin compile
Android unit tests
Android lint (zero errors)
Rust formatting for changed files
targeted Rust tests
release APK assembly
APK certificate comparison
adb install -r
installed package/version verification
```

Known non-blocking cleanup candidates:

- local research and screenshot artifacts are untracked and not packaged;
- release R8/minification remains disabled. Enabling it could reduce Kotlin,
  Ktor, and Netty bytecode, but Droid-MCP reflection/service discovery needs a
  dedicated keep-rule and device-regression pass; do not flip it during an
  unrelated release build;
- upstream Rust warnings and Android lint style/dependency warnings remain;
- dependency upgrades must be isolated because GameActivity/Android runtime
  changes have high regression risk;
- loopback-only Droid-MCP transport remains a future hardening project.

## Common Failure Patterns

- Do not keep `AccessibilityNodeInfo` across observations.
- Do not execute an element ID from an older revision.
- Do not send raw accessibility trees to the model.
- Do not use screenshots for ordinary buttons/textboxes.
- Do not use Crawl4AI for login state or proof that an action succeeded.
- Do not cancel an Agent response from acoustic evidence alone.
- Do not reload TTS for every sentence.
- Do not run voice capture or Phone Use server from a short-lived Activity.
- Do not create separate Bootstrap and Ubuntu credentials or Agent CLIs.
- Do not uninstall the app to test a release intended to preserve user data.
