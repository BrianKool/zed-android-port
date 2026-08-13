# Zdroid-B Conversation Handoff

Last updated: 2026-08-13

This document summarizes the long-running Zdroid-B development conversation and the current technical state of BrianKool's fork of `zed-android-port`.

The goal is to avoid re-spending time and tokens rediscovering decisions, bugs, fixes, and runtime rules.

## Repository And Branch State

Primary working repository:

- Public fork: `https://github.com/BrianKool/zed-android-port`
- Private working copy/release repo: `https://github.com/BrianKool/zdroid-b-private`
- Upstream Android port: `https://github.com/Dylanmurzello/zed-android-port`
- Zed upstream: `https://github.com/zed-industries/zed`

Important rule:

- Do not push to Dylanmurzello upstream.
- Do not push to Zed upstream.
- Current experimental/private work should go to `BrianKool/zdroid-b-private` unless Brian explicitly says otherwise.
- Do not commit, push, or upload release APK automatically after every change. Only do it when explicitly requested.

Current local branch used during this handoff:

- `fix/v1.1.2-agent-node`
- Tracking: `private/fix/v1.1.2-agent-node`

Recent important commits:

- `012730d6b0 docs(android): document runtime path and build rules`
- `f58954c561 fix(android): keep app git on bootstrap path in managed linux`
- `4c45adc50e fix(android): soften missing runtime git identity setup`
- `9efdb8d626 fix(android): unify runtime git and mobile agent state`

Private release used in the last phase:

- Tag: `v1.1.3-beta-1b`
- Release asset: `Zdroid-B-1.1.3-beta-1b.apk`
- Latest uploaded private asset size after Git runtime fix: around `313649447` bytes.

## Product Goal

Brian forked `zed-android-port` to build **Zdroid-B**, a practical Android IDE based on Zed/Zdroid that works well on:

- Samsung phone screen, about 360 logical px wide.
- Samsung DeX desktop-like window mode.
- Same-device development:
  - Zdroid-B as frontend/editor.
  - Local terminal/runtime.
  - AI agent panel using subscription-based CLIs where possible, not API-only billing.
  - Optional local LLM provider.
  - Optional Full Linux compatibility layer for Linux/glibc tools.

The app is intended first for Brian's personal use, then potentially release APK download from Brian's fork.

## Version Naming History

The conversation used many beta labels. Important convention:

- Early builds used `beta-1*`.
- Later first stable direction became `1.0.0`, `1.0.1`.
- Upstream sync branch became `1.1-beta-1a` and later `1.1`.
- Subsequent private/debug/test releases included `1.1.1`, `1.1.2`, `1.1.3`, and `1.1.3-beta-1b`.

Practical current rule:

- Treat `v1.1.3-beta-1b` in the private repo as the current tested private beta artifact at the end of this conversation.
- Do not assume public release pages are current unless verified.

## Major Functional Areas Implemented Or Modified

### 1. Android Runtime Picker / Setup Flow

The runtime setup evolved several times.

Current conceptual model:

- `Zdroid Standard`
  - Android-native, Termux-like Bootstrap environment.
  - Fast setup.
  - Uses Bionic/Termux-style packages.
  - Good for Git, npm, Node, CLI agents, credentials, app integration.

- `Zdroid Full Linux`
  - Includes Zdroid Standard plus Managed Linux / Ubuntu compatibility.
  - Uses PRoot/Ubuntu glibc userland.
  - Intended for Linux/glibc software and local servers.
  - Can take much longer to install, around 20 minutes.

Earlier runtime options included:

- Chroot rootfs with Magisk/zd-spawnd.
- Zdroid Bootstrap.
- Existing Termux app.
- Managed Linux / Ubuntu.
- Optional Alpine/musl compatibility.

Decision:

- For normal users, the first setup should not overwhelm them with multiple advanced runtime choices.
- `Zdroid Standard` and `Zdroid Full Linux` are the main public-facing options.
- Optional Alpine/musl should be advanced only, suggested when a musl binary is detected.

Important UX requirements:

- Storage permission and notification permission must be serialized, not requested simultaneously.
- Runtime picker should use the dialog template and not break permission/onboarding flow.
- Dialogs should generally be:
  - Minimum width: 80% of screen.
  - Minimum height: 50% of screen.
  - Maximum width/height: 90% of screen.
  - Content scrolls inside the dialog.
- Runtime picker must show realistic time estimates:
  - Standard setup around 5 minutes.
  - Full Linux setup around 20 minutes.
- Long setup tasks should show progress/loading and notification status.

### 2. Bootstrap / Package Manager Fixes

Many issues were around first-run package setup:

- `pkg update && pkg upgrade` initially failed or required `apt --fix-broken install`.
- Conflicts in package upgrades required dpkg force-conf options.
- `LD_PRELOAD` / Termux path rewrite was needed for package operations because upstream Termux packages reference `com.termux`.
- Package initialization after onboarding was added so first-run users are not expected to manually know terminal commands.

Important decisions:

- Do not call it "repair" in user-facing onboarding; users do not know what is being repaired.
- Phrase it as "initialisation", "setting up essential packages", or similar.
- Initialization may take around 10 minutes and should explicitly say it can continue in background.
- Failed package initialization should support retry.
- Background tasks should be visible in notification.

### 3. Background Execution / Notification

Desired behavior:

- Zdroid-B should behave closer to Termux:
  - One main persistent session notification.
  - Background execution enabled.
  - Optional wakelock/wifi lock.
  - User can leave app while long tasks run.
  - Completed tasks should notify user.

Implemented/attempted:

- Foreground/background notification support.
- Task completion notification.
- Wakelock / wifi lock discussions and some implementation work.

Open UX question:

- Whether task completion should be separate notifications or summarized in the main session notification.

Brian's preference:

- Main session notification should clearly show whether:
  - no tasks are running,
  - N tasks are running,
  - app is waiting for user response.

Future requirement:

- Detect AI response state:
  - If AI is asking a question / waiting for user answer, notification should say that.
  - This prevents Brian waiting outside the app thinking the agent is still working.

### 4. GitHub / Git / Credential Integration

Implemented/changed:

- GitHub login flow:
  - Token login.
  - GitHub device code / browser login.
  - Account display.
  - GitHub repository picker for clone.

- GitHub repository clone UX:
  - Repository URL mode.
  - GitHub account repository list mode.
  - Searchable repository list.
  - Better mobile-friendly layout.
  - Separate save directory block.
  - `Clone` and `Cancel` buttons.

- Git credential sharing:
  - GitHub account token should be reused by Git panel and terminal Git operations.
  - `gh` CLI should be able to share the same GitHub credential setup where possible.
  - Git author identity should be set from GitHub user when possible.

Important issue fixed:

- Git UI showed:

  ```text
  Git is not installed or not available in the active Zdroid runtime.
  Install Git in Zdroid Standard, or switch to Zdroid Full Linux and install Git there.
  ```

- Root cause:
  - The app had both Bootstrap and Managed Linux/Ubuntu.
  - Git panel discovered Bootstrap Git at:

    ```text
    /data/data/com.zdroid/files/usr/bin/git
    ```

  - In Managed Linux mode, the Android command bridge treated this app-private absolute path as something to route into Ubuntu.
  - It became:

    ```text
    /root/../usr/bin/git
    ```

  - That path does not exist inside Ubuntu.
  - Symptoms:

    ```text
    /bin/bash: line 1: /root/../usr/bin/git: No such file or directory
    git status failed
    git worktree list failed
    os error 2
    exit status 127
    ```

Fix:

- Managed Linux PATH now falls back to Bootstrap:
  - `zd-runtime`
  - app bridge `bin`
  - Bootstrap `.zed/bin`
  - Bootstrap `bin`
  - existing PATH

- GitBinary now has Android-specific safe handling:
  - If Git path is app-private Bootstrap Git (`/data/data/com.zdroid/files/usr/bin/git` or `/data/user/0/...`), it uses short name `git` instead of absolute path.
  - This lets PATH resolve Git correctly without forcing the path into Ubuntu/proot.

Validation:

- After the fix, logcat no longer showed:
  - `/root/../usr/bin/git`
  - `git status failed`
  - `git worktree list failed`
  - `Git is not installed`
  - `run git config --global ... os error 2`

Additional related fix:

- `ensure_git_identity` was changed from sync `new_std_command("git")` to async `new_command("git")`.
- Missing Git on Android now logs a warning rather than breaking app startup or GitHub auth:

  ```text
  GitHub is signed in, but Git is not available in the active Zdroid runtime; skipping automatic git config --global user.name
  ```

### 5. Runtime Path Rules Document

Created:

- `docs/ZDROID_RUNTIME_PATH_RULES.txt`

Purpose:

- Document Bootstrap vs Ubuntu path ownership.
- Explain why cross-runtime absolute paths are dangerous.
- Explain which tools should run in Bootstrap vs Ubuntu.
- Provide debug checklist for `No such file or directory`.
- Provide build reminder.

Important rule from that file:

- Bootstrap app-private paths:

  ```text
  /data/data/com.zdroid/files/usr/bin/git
  /data/data/com.zdroid/files/usr/bin/node
  /data/data/com.zdroid/files/usr/bin/npm
  /data/data/com.zdroid/files/home
  ```

- Ubuntu/Managed Linux paths:

  ```text
  /root
  /root/projects
  /usr/bin/git
  /usr/bin/node
  /bin/bash
  ```

- Never pass a Bootstrap absolute executable path into Ubuntu.
- Prefer short command names:

  ```text
  git
  node
  npm
  bash
  ssh
  gh
  claude
  codex
  ```

### 6. AI Agent Panel

Initial goal:

- Make Zed agent panel usable on Android with:
  - Codex CLI.
  - Claude CLI.
  - Avoid API key billing when user already has subscription login in terminal.

Agents/providers explored:

- Codex.
- Claude.
- Gemini.
- Grok.
- OpenCode.
- GitHub Copilot.
- Local LLM.

Final state near end of conversation:

- Keep only Codex and Claude as external subscription-style agents.
- Gemini was removed from active agent list because login/ACP flow was unreliable.
- Grok was also removed because official installer did not advertise Android support and auth flow was not useful.
- OpenCode/Copilot/Gemini/Grok were discussed but should not be active unless explicitly re-added later.

Important Codex fixes:

- Browser login token exchange repeatedly failed earlier:
  - `token_exchange_failed`
  - `error sending request for url (https://auth.openai.com/oauth/token)`
  - `chatgpt.com/backend-api/codex/responses`
  - DNS/IPv6/certificate transport issues.

- Later Codex login began working after multiple fixes:
  - Android context init crash addressed.
  - URL open handling adjusted.
  - Auto-open browser behavior later disabled because it created issues.

Important Claude fixes:

- Claude ACP had many failures:
  - `claude-agent-acp: not found`
  - native binary musl/glibc mismatch
  - `exit status 127`
  - `signal 9 (SIGKILL)`
  - `node.js and npm are required before claude can be installed`

- Final direction:
  - Claude CLI and ACP should use Bootstrap Node/npm and Bootstrap HOME.
  - Do not route Bootstrap Node/npm into Ubuntu.
  - Claude login action should run interactive `claude`, not `claude auth login --claudeai`, because the latter did not expose the paste-code flow properly.

Agent panel UX requirements/fixes:

- Dropdowns under prompt input should not automatically open the soft keyboard.
- Prompt input should open keyboard when directly focused.
- On mobile, subscription login should hide/close agent panel and open terminal so user sees what is happening.
- On DeX, it does not need to hide panel.
- Agent panel width/resizing should be available; fixed width caused problems.
- History icon behavior on mobile should route to Thread panel rather than separate history panel.
- Thread panel should hide, not close the app/main activity.
- Thread panel should have close button.
- Conversation history should allow delete/archive.
- Mobile thread list should avoid accidental open while scrolling:
  - first tap selects and reveals Archive/Delete,
  - second tap opens,
  - drag scrolls.
- Agent conversation text should support selection/copy.
- Conversation should be exportable/downloadable in the future.
- New chat via plus should not accidentally reload an old conversation.

Recent agent panel issues:

- Chat box minimization button and agent panel expand button conflict.
- If chat box is focused, pressing combinations of:
  - agent panel expand,
  - chat box minimize/expand,
  can make agent panel component fail to reopen until app restart.
- Button icon direction requirement:
  - chat box minimize: arrow down.
  - chat box expand: arrow up.
- Pressing planning-mode Submit should not close the agent panel or change focus unexpectedly.
- App crash/component crash occurred when submit + focus behavior was modified too aggressively.

Future requirement:

- Add notification state for "AI is waiting for user's answer".

### 7. Local LLM Provider

Implemented/attempted:

- Local LLM provider integrated into Zed LLM provider system.
- Local LLM placed near top of provider list, intended below Zed / high priority.
- Model download/install tasks with progress and notifications.
- Qwen 2.5 Coder 7B Q5_K_M.
- Google Gemma 3 4B IT Q4_K_M.
- Other model list ideas:
  - DeepSeek.
  - Google/Gemma.
  - Llama.
  - 7B and below preferred due to mobile limits.
- Delete downloaded model option.
- Delete confirmation should be popup warning dialog, not appended at bottom of model list.
- Downloaded models should appear at top.
- Searchable/dropdown model list.
- Add custom local LLM:
  - GGUF URL input.
  - Import local GGUF file.
  - SHA256 verification.

Runtime:

- Uses llama.cpp / llama-server style local OpenAI-compatible API.
- Local endpoint around:

  ```text
  http://127.0.0.1:8080/v1
  ```

Security note:

- `127.0.0.1:8080` has no auth and Android apps share network namespace.
- Other apps could potentially call the local LLM and consume resources.
- This was judged acceptable for now because Zdroid-B is developer-focused and not a mass consumer app.

Performance findings:

- Qwen 2.5 7B could respond but was extremely slow.
- Gemma 3 4B also felt extremely slow, not near expected 10 tokens/sec.
- Vulkan backend did not visibly improve speed.
- Phone got hot with little/no output.
- Zed Agent sends substantial context even for simple prompts.

Observed local LLM error:

```text
request (2939 tokens) exceeds the available context size (2048 tokens)
```

Explanation:

- This is a local llama-server API 400 error.
- Local LLM server enforces context size.
- Zed Agent preloads instructions/rules/context, so even a tiny prompt can exceed 2K context.

Future local LLM improvements requested:

- Show real-time:
  - tokens/sec,
  - context count,
  - token usage,
  - context growth over time.
- Show this for local LLM, Codex, and Claude conversation context if possible.
- Explain the Zed Agent preloaded context to users.
- Rename/clarify profiles:
  - `minimal`
  - `ask`
  - `write`
  Brian felt `write` is unclear and should be renamed to `agent` or explained in an info tab.
- Maybe create a simpler custom ACP/local agent later, but this may be less useful than Zed Agent because Zed Agent provides context/tooling.

Conclusion:

- Local LLM works technically but is currently too slow for serious agentic coding on phone.
- It remains useful as an experimental provider, but not as replacement for Codex/Claude.

### 8. UI / Mobile Responsive Design

Major UX requirements throughout:

- Every window/dialog/panel must be responsive on 360 logical px width.
- Avoid content going off-screen.
- Bottom bars must not hide dialog content.
- Dialog internal content should scroll.
- Dialogs should have close/back buttons.
- DeX should show centered floating dialog.
- Mobile can use modal overlay, but should not become a separate Android Activity when avoidable.

Fixed/attempted:

- Samsung DeX mouse capture issue:
  - Initially mouse was trapped in Zdroid window.
  - Fixed by disabling captured pointer behavior in DeX/mobile contexts.
  - Later pointer offset issues were adjusted.

- Hover behavior disabled/reduced for mobile.

- Soft keyboard behavior:
  - Do not auto-open keyboard in code editor because there is a keyboard toggle.
  - Do auto-open keyboard in settings/dialog text inputs and clone repository fields.
  - Terminal should follow Termux-like behavior.
  - Terminal input line must stay above keyboard.
  - Terminal should not be covered when resized large.
  - Leaving terminal should clear custom key modifiers like Ctrl.

- Terminal extras row:
  - Add ESC, CTRL, ALT, arrows, HOME, END, PGUP, PGDN similar to Termux.
  - Buttons were initially too large.
  - Ctrl needed double tap and leaked into other inputs; fix requirement:
    - clear terminal custom modifiers whenever leaving terminal.

- Text selection:
  - Code editor long press should select text like normal Android apps:
    - tap = cursor,
    - long press = selection handles,
    - context menu with cut/copy/paste/more.
  - Terminal should support select/copy/paste.
  - AI conversation should support select/copy.

- Tabs:
  - Long filenames make it hard to reach close button.
  - Need better tab close UX.

- Project name:
  - Workspace/project name in header should be larger and bold.
  - Border/oval was tested and later Brian preferred no border, just bold and larger.

- Welcome page:
  - Remove configure block because settings/keymaps/extensions are accessible via top menus.
  - Add fork/version info in Settings -> General bottom:
    - `zdroid-b version`
    - fork GitHub URL.
  - Welcome agent info should include command examples with copy/play buttons.
  - Commands should not use `&&` if long/complex; separate actions are better.

### 9. Dialog Template

Repeated issue:

- Many dialogs were too short, too narrow, transparent, cut off, or had double frames.

Affected dialogs:

- Runtime picker.
- Settings.
- GitHub account dialog.
- GitHub repository clone dialog.
- Trust project dialog.
- Unrecognized project / trust directory dialog.
- API key dialog.
- Agent info panel.

Desired template:

- Minimum width: 80% of screen.
- Minimum height: 50% of screen.
- Maximum width: 90% of screen.
- Maximum height: 90% of screen.
- Content determines size between min and max.
- If content exceeds max, scroll inside dialog.
- Must have close or back button.
- Background overlay lower opacity.
- Avoid two nested border frames unless required.

Important clarification:

- 50% height is minimum, not fixed height.
- Dialog with lots of content should grow up to 90% height.

### 10. Project Open / Clone Flow

Issues fixed/attempted:

- `Open Project` did nothing.
- Open project selected folder but did not import/open.
- Importing project had no loading/progress.
- Runtime picker or folder picker flow accidentally imported when only selecting save directory.
- Clone repository GitHub directory picker did not preserve selected directory.
- Error message could appear off-screen.

Requirements:

- Selecting save directory in clone repository must not trigger project import.
- Clone should show popup/toast error if failed.
- Error must be visible, red-ish, and not off-screen.
- Open project should show loading/progress.
- If project needs trust, dialog must be readable and full width enough.
- Folder picker should preferably show only Zdroid project roots, not irrelevant folders.

### 11. File Tree Operations

Requested:

- Long press folder:
  - New File.
  - New Folder.

- Long press file:
  - Rename.
  - Move.
  - Cut/Paste.

Decision:

- Preserve DeX mouse double click to open file.
- Use Android touch long press for context menu.

Issue:

- New file creation caused a momentary layout deformation/visual glitch.

### 12. Git Changes Panel

Issues/fixes/requirements:

- Commit input field keyboard did not appear initially; later found there was a hidden button.
- Git commit failed with:

  ```text
  Author identity unknown
  Please tell me who you are
  ```

- Fix direction:
  - GitHub login should auto populate global Git identity if possible.

- Dubious ownership:
  - Git panel showed:

    ```text
    Detected dubious ownership in repository ...
    Trust Directory
    ```

  - Trust Directory button initially did nothing or produced `os error 2`.
  - Later Git runtime fixes targeted this.

- Changes panel future requirement:
  - Long press changed file to rollback selected file.
  - Multi-select rollback.
  - Stash selected files.
  - More than only "stash all".

### 13. AI Provider Settings After Upstream Sync

After merging/syncing many upstream commits, settings UI changed:

- AI provider settings moved/integrated into broader app settings.
- Several provider UI layouts broke on mobile:
  - DeepSeek API key text stacked vertically.
  - Anthropic API key text stacked vertically.
  - Local LLM was hard to find.

Fix direction:

- Mobile provider UI needs responsive widths.
- Local LLM should remain visible near top.
- Mobile settings dialog must respect dialog template.

### 14. Upstream Sync / 1.1 Integration

At one point upstream had about 1498 commits.

Brian asked to:

- Integrate upstream carefully into `integration/zed-v1.16`.
- Use strict mode.
- Do not delete Zdroid-B features silently.
- Prefer Zdroid-B UI/responsive design when conflicts arise.
- If a merge decision is unclear, ask.
- Do not merge upstream changes that are not worth it; keep Brian's version if better.

Outcome:

- A 1.1-style branch/release incorporated upstream changes.
- Some upstream settings/provider changes broke mobile UI and required follow-up fixes.

Important:

- Brian's fork may show "ahead/behind" relative to Dylanmurzello because Brian has custom commits and upstream has many new commits.
- This is expected.

### 15. Android Package ID / Signing

Package ID was discussed:

- There was a phase trying `com.zdroid.b`.
- This caused path/bootstrap issues because Termux bootstrap paths and patches were tied to 12-char package assumptions.
- Decision returned to `com.zdroid` for compatibility.

Signing:

- Debug signing and release signing were used at different times.
- Brian created release keystore/signing properties.
- Important rule:
  - Use same release key for updates.
  - Changing signing key requires uninstall/reinstall and can lose app data.
  - Do not create a new keystore casually.

### 16. APK Size

APK grew to about 300 MB.

Reasons:

- Rust native library.
- Android assets.
- Zed codebase.
- Bundled runtime helpers.

Potential optimization was requested for 1.0.1:

- Remove unsafe/redundant code.
- Optimize project size.
- Clean unused assets/code.

But caution:

- Do not accidentally remove upstream Zed or Zdroid code that is still needed.

### 17. MongoDB / Managed Linux Compatibility

Brian wants Zdroid-B to make the phone a powerful development computer, including local services like MongoDB.

Important discussion:

- `npm install mongodb` installs the Node driver, not MongoDB server.
- Local MongoDB server requires compatible binary/runtime.

Linux binary compatibility concepts explained:

- Linux ARM64 glibc:
  - Normal Ubuntu/Debian-style Linux binary.
  - Needs glibc loader.

- Linux ARM64 musl:
  - Alpine/musl binary.
  - Needs musl loader.

- Android ARM64 Bionic:
  - Native Android libc binary.
  - Runs in Bootstrap/Android side.

- Static ARM64 binary:
  - May run more portably if truly static and compatible.

Decision direction:

- True user freedom requires runtime compatibility, not just prompting agents.
- Priority order suggested:
  1. Universal glibc/musl/Bionic ELF detection and execution.
  2. Managed Linux Runtime.
  3. Unified HOME/PATH/project/credentials.
  4. Generic package installer.
  5. Generic background service manager.
  6. Validate with MongoDB, PostgreSQL, Redis.
  7. Agent prompt and safety rules last.

Safety prompt idea:

- Agents should be instructed:
  - Do not treat Zdroid-B as Ubuntu.
  - Do not install PRoot unless user explicitly confirms.
  - Do not modify project-external environment unless confirmed.
  - Before installing services, check executable, ABI, port, and disk needs.
  - If server is missing, do not only create a connection string.
  - If Linux binary is incompatible, list native/musl/glibc/PRoot options.

But conclusion:

- Prompt is not enough. Runtime compatibility layer is the real solution.

### 18. DNS / Certificates / Network

Issues observed:

- MongoDB SRV lookup:

  ```text
  querySrv ECONNREFUSED _mongodb._tcp.weddingquiz.feidpjg.mongodb.net
  ```

- Codex/ChatGPT stream warning:

  ```text
  Warning: Falling back from WebSocket to HTTPS transport.
  stream disconnected before completion: invalid peer certificate: unknownIssuer
  ```

- Codex login token exchange errors.

Implemented/attempted:

- Android DNS bridge writes active DNS servers to app-private resolv.conf.
- CLI launchers inherit resolver config.
- DNS/certificate fixes were discussed and partially patched.

Future caution:

- Other APIs like Google Maps, databases, OAuth, and WebSocket endpoints may also expose DNS/certificate edge cases.

### 19. Android Offline Launch Bug

Brian observed:

- Zdroid-B crashed/opened incorrectly when Wi-Fi/cellular were off.

Requirement:

- App should not require network to open.
- Network-dependent checks must be best-effort and not crash startup.

Status:

- Investigation was started, but exact final fix is not fully summarized here.
- Keep this in regression testing.

### 20. Browser / URL Opening

Issues:

- Codex OAuth local server used `localhost:1455`.
- Android broadcast to open URL showed:

  ```text
  Permission Denial: broadcast asks to run as user -2 ...
  ```

- It did not block login, but was noisy.

Decision:

- Eventually auto-open browser for agent login was disabled for all agents because Gemini/other login flows produced broken behavior and confusion.
- Terminal should show login URL/code clearly.
- Later requirement:
  - terminal selected website/context menu should include open browser option.

### 21. Gemini / Antigravity / Grok

Gemini:

- Tried adding Gemini CLI/ACP style agent.
- Issues:
  - `prefix: parameter not set`
  - login did nothing
  - missing files under `.local/share/zdroid/gemini/...`
  - exit status 41
  - Google auth URL 404
  - Gemini CLI indicated changed direction toward Antigravity API.

Decision:

- Remove Gemini agent for now.

Antigravity:

- Brian asked if Antigravity can be like Codex/Claude ACP.
- Researched/discussed:
  - Official Antigravity ACP support unclear/not stable.
  - A repo `shubzkothekar/antigravity-acp` existed but should not be adopted blindly.
  - Future may be possible to build `antigravity-agent-acp` like Claude/Codex wrappers.

Decision:

- Do not add Antigravity now.
- Wait for clearer ACP/community support.

Grok:

- Zed has Grok Build ACP documentation.
- Tried adding Grok.
- Issues:
  - authenticating stuck.
  - terminal opened then closed.
  - official installer did not advertise Android support.

Decision:

- Remove Grok for now.

### 22. Model Names / Usage Display

Brian wanted:

- Claude model versions like:
  - Opus 4.6
  - Opus 4.7
  - Opus 4.8
  - Opus 5
  - Fable 5

- Codex usage:
  - 5-hour window remaining.
  - reset time.
  - weekly usage remaining.

- Claude usage if possible.

Observation:

- ACP/server returned limited model labels like `opus`, `haiku`, sometimes `sonnet`.
- JetBrains cc-gui can show more because it may query or maintain model lists differently.

Status:

- Not fully solved.
- Do not assume ACP exposes all model versions.
- Adding hardcoded labels may misrepresent actual backend model.

Brian also asked if selecting `GPT-5.5` really uses that model:

- This depends on Codex ACP backend honoring the selected config option.
- UI label alone is not proof.
- Need backend logs/request inspection or provider confirmation to be certain.

### 23. Sandbox

Upstream added sandbox-related code.

Brian asked:

- Does upstream Sandbox apply to Zdroid-B?
- Should mobile disable sandbox?

Discussion:

- Sandbox is upstream Zed desktop feature.
- On Android mobile, much of it may not be useful or may be confusing.
- Requirement:
  - Sandbox UI/features that are irrelevant on mobile should be disabled/hidden.

Status:

- Some sandbox code remains in build warnings as unused.
- Do not assume it is fully usable on Android.

### 24. Build Process And Common Build Problems

A build reminder was added to:

- `docs/ZDROID_RUNTIME_PATH_RULES.txt`

Important build rules:

- Correct Android Gradle working directory:

  ```text
  crates/gpui_android/examples/zed_android/android
  ```

- Use cached Gradle:

  ```powershell
  C:\Users\brian\.gradle\wrapper\dists\gradle-8.11.1-all\2qik7nd48slq1ooc2496ixf4i\gradle-8.11.1\bin\gradle.bat :app:assembleRelease --no-daemon
  ```

- Do not assume system `gradle`.
- Do not assume `gradlew.bat` exists.

Pre-build checks:

```powershell
cargo --version
rustc --version
cargo ndk --version
adb devices
```

Formatting:

```powershell
rustfmt --edition 2024 --check <files>
```

After build:

- Confirm APK timestamp changed:

  ```powershell
  Get-Item crates\gpui_android\examples\zed_android\android\app\build\outputs\apk\release\app-release.apk
  ```

- Do not install an old APK after a timeout.
- Check Java/Gradle orphan processes:

  ```powershell
  Get-Process java,gradle -ErrorAction SilentlyContinue | Select-Object ProcessName,Id,CPU,StartTime
  ```

- Inspect Java command lines if stuck:

  ```powershell
  Get-CimInstance Win32_Process -Filter "name='java.exe'" | Select-Object ProcessId,CommandLine | Format-List
  ```

- Stop Gradle cleanly:

  ```powershell
  C:\Users\brian\.gradle\wrapper\dists\gradle-8.11.1-all\2qik7nd48slq1ooc2496ixf4i\gradle-8.11.1\bin\gradle.bat --stop
  ```

Post-install smoke test:

```powershell
adb logcat -c
adb shell am force-stop com.zdroid
adb shell monkey -p com.zdroid 1
adb logcat -d -t 3000
```

Search for:

```text
No such file
os error 2
exit status 127
/root/../usr/bin
Git is not installed
git status failed
node missing
zed-npm
claude
codex
```

### 25. Current Known Good State At End Of Conversation

Installed on phone:

- Latest locally built `v1.1.3-beta-1b` private APK after Git runtime fixes.

Verified:

- App launches.
- Git repo opens.
- Logcat no longer shows Git path translation bug:
  - no `/root/../usr/bin/git`
  - no `git status failed`
  - no `git worktree list failed`
  - no `Git is not installed`
  - no `run git config --global ... os error 2`

Committed/pushed:

- Runtime path/build doc.
- Git identity softening fix.
- Git app-private path/ManagedLinux fallback fix.

Not automatically done:

- No public release upload unless explicitly requested.
- No push to upstream.

## Important Regression Tests

When continuing development, test these on phone, not just desktop build:

1. First install after uninstall:
   - Permission flow.
   - Runtime setup.
   - Onboarding.
   - Initialization.

2. Runtime picker:
   - Standard install.
   - Full Linux install/repair.
   - Dialog size and scroll.
   - Notification during long install.

3. Git:
   - Open existing repo.
   - Trust directory.
   - Git changes panel.
   - Commit.
   - Push.
   - Terminal `git` command.
   - `gh` command if installed.

4. AI:
   - Codex login from terminal.
   - Codex appears in agent panel.
   - Claude login from terminal.
   - Claude appears in agent panel.
   - New conversation is actually new.
   - History/thread opens on mobile without closing app.
   - Chat box minimize/expand does not break agent panel.

5. Terminal:
   - Keyboard opens only when terminal focused.
   - Terminal input line stays above keyboard.
   - Custom key row works.
   - Ctrl/Alt states clear when leaving terminal.
   - Select/copy/paste.

6. Mobile dialogs:
   - Settings.
   - GitHub accounts.
   - GitHub repository clone.
   - Runtime picker.
   - Trust project.
   - API key dialog.
   - Agent info.

7. DeX:
   - Mouse not trapped.
   - No pointer offset.
   - Scroll wheel works.
   - Dialogs are floating and centered.

8. Local LLM:
   - Provider appears.
   - Download model.
   - Delete model confirmation.
   - Context/token/tps display if implemented.
   - Avoid context overflow.

9. Offline:
   - App opens with Wi-Fi/cellular disabled.

## Open Problems / Future Work

High priority:

- Agent panel chat box minimize/expand conflict when chat box is focused.
- Planning-mode Submit should not close or break agent panel.
- Notification should show when AI is waiting for user.
- Thread/history mobile double-action behavior still needs exact UX verification.
- Conversation text selection/copy/export.
- Dialog template consistency across all settings/provider dialogs.

Runtime:

- General cross-runtime command ownership beyond Git:
  - node
  - npm
  - gh
  - ssh
  - claude
  - codex
  - LSPs
  - formatters

- Managed Linux service manager.
- MongoDB/Postgres/Redis validation.
- Better ELF compatibility detection.
- Optional musl/Alpine advanced install.

Local LLM:

- Performance remains poor.
- Vulkan not clearly helping.
- Need live t/s/context/token usage display.
- Need better context budget handling for Zed Agent.

Git/GitHub:

- Ensure GitHub credentials are shared cleanly with terminal, Git panel, and `gh`.
- Private release workflow/documentation.

Build/release:

- Keep private/public repo separation clear.
- Do not upload release unless requested.
- Maintain same release signing key.

## Key Engineering Principles Established

1. Bootstrap is the app integration and credential layer.
2. Ubuntu/Managed Linux is the Linux/glibc compatibility layer.
3. Do not pass Bootstrap absolute paths into Ubuntu.
4. Prefer short command names and PATH resolution.
5. If absolute paths are unavoidable, they must carry runtime ownership.
6. Mobile UI must be tested on phone, not assumed from desktop.
7. Dialogs must fit 360 logical px mobile width.
8. No hidden async behavior for normal UI actions:
   - If user taps Settings, it should appear immediately.
   - If work is loading, show loading.
9. Long tasks need foreground notification and clear status.
10. Do not automatically push/upload unless Brian explicitly asks.

## Useful Files Mentioned

- Runtime/path/build rules:
  - `docs/ZDROID_RUNTIME_PATH_RULES.txt`

- This handoff:
  - `docs/ZDROID_B_CONVERSATION_HANDOFF.md`

- Runtime picker:
  - `crates/gpui_android/examples/zed_android/src/runtime_picker.rs`

- Android app main:
  - `crates/gpui_android/examples/zed_android/src/lib.rs`

- Managed Linux adapter:
  - `crates/zdroid_runtime/src/adapters/managed_linux.rs`

- Bootstrap adapter:
  - `crates/zdroid_runtime/src/adapters/bootstrap.rs`

- Runtime provider trait:
  - `crates/zdroid_runtime/src/port.rs`

- Git binary logic:
  - `crates/git/src/repository.rs`

- Git UI / GitHub auth:
  - `crates/git_ui/src/github_auth.rs`
  - `crates/git_ui/src/git_ui.rs`

- Filesystem Git operations:
  - `crates/fs/src/fs.rs`

- Android runtime bridge:
  - `crates/gpui_android/native/zd-runtime/zd-exec`
  - `crates/gpui_android/src/zd_exec_install.rs`

## Final Note

The most important lesson from this conversation:

Zdroid-B is not just "Zed on Android". It is a multi-runtime IDE where Android-native Bootstrap and Linux/Ubuntu compatibility coexist. Most subtle bugs come from failing to preserve runtime ownership across paths, tools, credentials, and UI flows.

When something says "not found" even though the user installed it, first ask:

> Which runtime owns this executable path?

That question solved the final Git issue and should guide future fixes.
