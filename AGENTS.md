# Zdroid-B AI Agent Guardrails

This repository is Brian's Zdroid-B fork of the Zed Android port. Before making
changes, every AI agent must read and preserve the product decisions below.

If a requested implementation appears to conflict with these decisions, stop
and ask Brian. Do not silently "simplify" the architecture.

## Non-Negotiable Product Decisions

### One App Identity

- The Android package id is `com.zdroid`.
- Do not rename it to `com.zdroid.b`.
- The embedded Bootstrap runtime, shebangs, RUNPATHs and staged paths depend on
  this package path.

### One User Experience, Two Internal Runtimes

Zdroid-B may internally use:

- Android Bootstrap: Termux-like, Bionic, under `/data/data/com.zdroid/files/usr`.
- Full Linux / Managed Linux: Ubuntu/glibc through PRoot.

Users should not have to understand or manually juggle these runtimes during
normal development.

The intended product experience is:

- one terminal experience;
- one project home;
- one Git/GitHub credential story;
- one Codex/Claude login story;
- one Agent Panel experience.

### Codex And Claude Must Be Shared

Codex and Claude terminal usage and Agent Panel usage must use the same
Zdroid-managed subscription login and launcher path.

Do not create a second independent Codex or Claude installation in Ubuntu just
to make Full Linux terminal commands work.

Correct direction:

- Bootstrap owns the managed Codex/Claude launchers and credentials.
- Agent Panel uses those launchers.
- Bootstrap terminal uses those launchers.
- Full Linux `/usr/local/bin/codex` and `/usr/local/bin/claude` should bridge
  back to the same launchers.

Wrong direction:

- installing a separate Ubuntu-native `@openai/codex`;
- installing a separate Ubuntu-native Claude CLI;
- creating separate credentials for terminal vs Agent Panel;
- fixing one runtime by splitting the user's account state into two worlds.

### Managed Executables Do Not Belong In HOME

Do not generate Zdroid-managed executable launchers in:

```text
/data/data/com.zdroid/files/home/.local/bin
```

Use app-managed executable locations instead:

```text
/data/data/com.zdroid/files/usr/bin
/data/data/com.zdroid/files/usr/.zed/bin
```

`$HOME` is shared with projects, user-installed tools, caches and credentials.
It is not the correct place for app-managed executable wrappers.

### Avoid Cross-Runtime Absolute Paths

Never pass a Bootstrap absolute executable path into Ubuntu:

```text
/data/data/com.zdroid/files/usr/bin/git
/data/data/com.zdroid/files/usr/bin/node
/data/data/com.zdroid/files/usr/bin/npm
```

Prefer short command names:

```sh
git
gh
node
npm
codex
claude
python3
pip
graphify
```

Short names allow Zdroid-B to choose the correct runtime through PATH and bridge
rules.

### Git And GitHub Credentials Must Be Shared

The Git UI, terminal `git`, terminal `gh`, clone UI and GitHub account UI should
use one coherent credential story.

- Do not make terminal Git and Zed Git panel require separate GitHub login flows.
- Do not hide clone failures behind only `exit status 128`; include Git stderr.
- Do not run Bootstrap absolute Git paths inside Ubuntu.

### Full Linux Is Compatibility, Not A Replacement Product

Ubuntu/Managed Linux exists for Linux/glibc compatibility: databases, servers,
Python native packages, apt tools and glibc-only binaries.

Bootstrap remains the app integration layer for:

- Codex/Claude managed launchers;
- GitHub credentials;
- Android browser/OAuth bridge;
- Zed app integration;
- runtime repair and package bootstrap.

### Mobile UX Has Priority

Zdroid-B is primarily used on phones and Samsung DeX.

Keep these decisions:

- dialogs must be responsive and use the shared dialog template;
- mobile panels should avoid accidental close/hide behavior;
- scrolling lists must not accidentally select items;
- keyboard behavior must be focus-aware;
- Agent Panel dropdowns must not summon the keyboard;
- terminal extra keys must not leak modifier state into editor/Agent Panel;
- phone users need clear progress and notifications for long tasks.

### Local LLM Is A Provider, Not An ACP Agent

Local LLM support should live in the language model provider system and talk to
local `llama-server` through OpenAI-compatible HTTP.

Do not turn local GGUF models into separate ACP agents unless Brian explicitly
changes this decision.

### Do Not Re-Add Removed Agents Casually

The active subscription ACP scope is currently Codex and Claude.

Gemini, Copilot, OpenCode and Grok were removed from the active agent list due
to compatibility and login/ACP uncertainty. Do not re-add them without a clear
new requirement and validation plan.

## Required Pre-Change Checklist

Before changing runtime, terminal, Git, credentials, Agent Panel or local LLM
code, check:

1. Does this split a previously shared login or credential path?
2. Does this introduce a second Codex/Claude installation?
3. Does this write app-managed executable wrappers into `$HOME`?
4. Does this pass `/data/data/com.zdroid/files/usr/...` into Ubuntu?
5. Does this make mobile UI less responsive or harder to close/back out of?
6. Does this hide errors that users need to diagnose?
7. Does this require users to understand Bootstrap vs Ubuntu for normal use?

If the answer is yes, redesign or ask Brian.

## Required Verification

For runtime/terminal/agent changes, verify at least:

```sh
which codex
which claude
which git
which gh
codex --version
claude --version
git --version
gh --version
```

Verify from both:

- Bootstrap terminal;
- Full Linux terminal.

Also verify Agent Panel can still start Codex/Claude and read the same
subscription login.

## Reference Documents

Read these before touching the relevant area:

- `docs/ZDROID_RUNTIME_PATH_RULES.txt`
- `docs/ZDROID_B_TERMINAL_ENVIRONMENT.md`
- `docs/ZDROID_B_TECH_STACK.md`
- `docs/ZDROID_B_CONVERSATION_HANDOFF.md`
- `docs/ZDROID_PHONE_BROWSER_VOICE_AUDIT.md`

## Default Engineering Rule

Preserve Zdroid-B decisions first. Upstream Zed and upstream zed-android-port
changes are valuable, but they do not automatically override Zdroid-B's mobile
UX, runtime bridge, credential sharing or subscription-agent decisions.
