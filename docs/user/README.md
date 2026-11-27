# Onicode User Guide

This guide explains how Onicode works, what you need before starting, what to expect, and where to find documentation for every feature.

## How Onicode works

Onicode is an open-source control plane for coding agents. It gives the coding agents already installed on your computer (or server) one fast interface on **desktop** and **mobile** (iOS and Android).

### Core concepts

- **Host (environment) and clients**: An Onicode **server** runs on the machine that holds your code and provider CLIs (your laptop, desktop, workstation, or VPS). Your desktop app and phone are **clients** connected to one or more environments.
- **Onicode is not a harness**: It drives the agent harnesses you already have: Claude Code, Codex, Cursor, Grok Build, Google Antigravity, OpenCode, Kiro, Command Code, Pi, Muse Code, fx, or any [ACP](./providers-acp.md) agent. You can switch providers or models mid-thread and let one agent delegate tasks to another.
- **Projects**: A project points to a folder on an environment (a single Git repository, a [folder with several repositories](./multi-repo-projects.md), or a plain folder). You can also start threads [without a project](./thread-sidebar.md#start-without-a-project).
- **Threads and worktrees**: Each task gets its own **thread**. Run a thread in the project's main checkout, or choose **New worktree** to give it its own Git branch and working directory, so several agents can work in parallel without stepping on each other.
- **Permission modes**: Choose per thread whether an agent asks before acting (**Supervised**), approves file edits itself (**Auto-accept edits**), uses the provider's automatic review (**Auto**), or runs without prompts (**Full access**).

## Before you start

- **Bring your own subscription or API keys**: Onicode is free and does not resell tokens. You need at least **one installed, signed-in provider** on the host machine before starting a thread. Codex (with ChatGPT) and Google Antigravity can be installed and signed in from Onicode itself.
- **Your code stays on your machines**: Code and prompts go from your host straight to the provider you chose. Other devices reach the host over your local network or your Tailscale tailnet; only [mobile notifications](./mobile-notifications.md) pass through a push gateway.
- **Your host must stay awake**: Your phone is a remote control. If the host sleeps or shuts down, agents pause until it wakes. Use the [background service](./background-service.md) for a host that is always on.
- **Git is optional but unlocks the best features**: Branches, worktrees, turn diffs, and pull requests need a Git repository (and a signed-in forge CLI or token for pull requests and issues).
- **Frontier agents first**: Onicode is built for frontier coding agents. OpenAI-compatible local endpoints such as Ollama can still handle light [text generation](./text-generation.md) like thread titles.
- **Device and server settings**: Appearance and keybindings belong to each device. Provider setup, thread defaults, worktree cleanup, and scheduled tasks belong to each environment, with per-project overrides.

## Quickstart

1. **Install Onicode** on the machine where your agents will run ([Install Onicode](./install.md)).
2. **Set up at least one agent** on the [Welcome page](./welcome-page.md) or in **Settings → Providers** (for example, `claude auth login` or `codex login`).
3. **Open or clone a project** and start a thread ([Working with threads](./thread-sidebar.md)).
4. **Optional: connect your phone or another computer**. Run `onicode up` on the host, or set **Settings → Sharing & access → Network access** in the desktop app ([Remote access](./remote-access.md)).

---

## Documentation index

### Getting started

- [Install Onicode](./install.md): the desktop app, the `onicode` command, the mobile app, WSL, and provider setup.
- [Welcome page](./welcome-page.md): first-run setup and importing existing Claude, Codex, and Pi sessions.
- [FAQ](./faq.md): common questions and troubleshooting.
- [Threads from older Onicode versions](./thread-migration.md): moving threads to the current database and recovery backups.

### Everyday work

- [Working with threads](./thread-sidebar.md): starting, pinning, settling, snoozing, and archiving threads, and side chats.
- [Messages and context](./composer.md): formatting, attachments, sending while the agent works, `@` mentions, the prompt stash, and editing an earlier prompt.
- [Permission modes](./permission-modes.md): Supervised, Auto-accept edits, Auto, Full access, and how they differ by provider.
- [Diffs and workspace files](./diffs.md): turn diffs, branch comparisons, commits, the file viewer, and review comments.
- [Source control](./source-control.md): GitHub, GitLab, and other forges, pull requests, and issues.
- [Activity log](./activity-log.md): inspecting tool calls and hiding work logs.
- [Subagents and delegation](./subagents.md): delegated threads and the subagents providers start themselves.
- [Context in portable handoffs](./portable-handoffs.md): how a conversation carries over when you switch provider or fork.
- [Files in question answers](./question-attachments.md): attaching files and images when you answer an agent's question.
- [Terminal](./terminal.md): host terminals, history, and shells.

### Workspace tools and media

- [Built-in browser](./browser.md): preview tabs, profiles, viewports, the element picker, annotations, and recordings.
- [Browser use](./browser-use.md): how agents browse, and which browser they drive.
- [Computer use](./computer-use.md): letting agents operate desktop apps.
- [Import browser sessions](./browser-import.md): copying sign-ins from your browsers into the desktop app's browser.
- [Use Onicode as your default browser](./default-browser.md): opening links and HTML files in Onicode on macOS.
- [Devices](./devices.md): iOS Simulators, Android Emulators and devices, and SSH device hosts.
- [Visual replies](./html-renders.md): inline HTML charts, diagrams, and mockups, and MCP apps.
- [SnapShots](./snap-shot.md): capturing a desktop window into a draft.
- [Voice input](./voice-input.md): dictation and transcript cleanup.
- [Prompt links](./prompt-links.md): links that open a prefilled draft or a thread.

### Projects, automation, and customization

- [Settings and project overrides](./project-settings.md): how settings inherit, pinning and archiving projects, worktrees, scheduled tasks, and webhooks.
- [Project file (`onicode.json`)](./onicode-json.md): checked-in project actions and worktree defaults.
- [Projects with several repositories](./multi-repo-projects.md): folders with several repositories, outside folders, submodules, and custom worktree layouts.
- [Workflows](./workflows.md): durable multi-stage workflows (Labs) with schedules, webhooks, and approvals.
- [Text generation models](./text-generation.md): fast, cheap models for titles, commit messages, and pull request descriptions.
- [Appearance and themes](./appearance.md): themes, custom CSS, math, and environment themes.
- [Keybindings](./keybindings.md): shortcuts, `keybindings.json`, `when` conditions, and hardware keyboards.
- [Keyboard focus](./keyboard-focus.md): moving around the app from the keyboard.

### Providers

- [Codex](./providers-codex.md): ChatGPT sign-in, several accounts, image generation, and Daybreak.
- [Claude](./providers-claude.md): several accounts, compaction, skills, MCP sign-in, Claude in Chrome, and routers.
- [Cursor](./cursor.md): signing in and what Cursor supports.
- [Grok Build](./providers-grok.md): Grok CLI setup, `XAI_API_KEY`, and permission modes.
- [Antigravity](./providers-antigravity.md): the managed runtime, sign-in methods, skills, and subagents.
- [OpenCode](./providers-opencode.md): OpenCode 2, local or external servers, compaction, and approvals.
- [Kiro](./providers-kiro.md): custom agents and permission modes.
- [Command Code](./providers-command-code.md): setup, models, and permission modes.
- [Pi](./providers-pi.md): setup, what carries over, and permission modes.
- [Muse Code](./providers-muse.md): sign-in, API keys, and limits.
- [fx](./providers-fx.md): backends, effort, and permission modes.
- [ACP providers](./providers-acp.md): ACP Registry agents and local ACP commands.

### Remote, mobile, and integrations

- [Remote access](./remote-access.md): pairing over your LAN or tailnet, Tailscale HTTPS, public access through Tailscale Funnel, desktop-managed SSH, remote desktop, and linked environments.
- [Mobile notifications](./mobile-notifications.md): push alerts, Live Activities, and Android live updates.
- [Outside agents](./outside-agents.md): connecting Claude Code, Codex, ChatGPT, or other apps to Onicode's MCP server with OAuth sign-in, and `onicode thread` scripts on the host.
- [Secrets](./secrets.md): secret references and secrets agents ask you for.
- [1Password service account](./secrets-1password-service-account.md): 1Password on a server without a desktop, with a service account token.
- [1Password Connect](./secrets-1password-connect.md): 1Password on a server without a desktop, with a Connect server.

### Operations and administration

- [Running Onicode in the background](./background-service.md): the background service, its environment, memory protection on Linux, and reclaiming database space.
- [Updating Onicode](./updating.md): updating the desktop app, servers, and providers, and continuing threads after a restart.
- [Usage and limits](./usage.md): token and cost tracking, custom prices, subscription limits, and quota sources.
- [Maintenance, power, and diagnostics](./diagnostics.md): background activity, keeping the host awake, the Diagnostics view, and `onicode trace`.
- [CLI reference (`onicode`)](./cli.md): an overview of the `onicode` commands.
- [Triage an issue](./triage.md): letting an agent investigate an Onicode problem and draft an issue.
- [Managed policy](./managed-policy.md): configuring Onicode for everyone on a machine.
- [Open source licenses](./open-source-licenses.md): third-party license notices in the app.
