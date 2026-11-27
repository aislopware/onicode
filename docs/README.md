# Onicode docs

## Using Onicode

New to Onicode? Start with the [user guide](./user/README.md): how Onicode works, what you need,
and every feature guide.

- [Install Onicode](./user/install.md)
- [Messages and context](./user/composer.md)
- [Working with threads](./user/thread-sidebar.md)
- [Permission modes](./user/permission-modes.md)
- [Diffs and workspace files](./user/diffs.md)
- [Subagents and delegation](./user/subagents.md)
- [Terminal history](./user/terminal.md)
- [Source control](./user/source-control.md)
- [Project settings](./user/project-settings.md)
- [Project file (`onicode.json`)](./user/onicode-json.md)
- [Projects with several repositories](./user/multi-repo-projects.md)
- [Appearance and themes](./user/appearance.md)
- [Keyboard shortcuts](./user/keybindings.md)
- [Voice input](./user/voice-input.md)
- [Text generation models](./user/text-generation.md)
- [SnapShots](./user/snap-shot.md)
- [Visual replies](./user/html-renders.md)
- [Built-in browser](./user/browser.md)
- [Import browser sessions](./user/browser-import.md)
- [Use Onicode as your default browser](./user/default-browser.md)
- [Devices](./user/devices.md)
- [Computer use](./user/computer-use.md)
- [Browser use](./user/browser-use.md)
- [Usage and limits](./user/usage.md)
- [Secrets](./user/secrets.md)
- [Remote access](./user/remote-access.md)
- [Mobile notifications](./user/mobile-notifications.md)
- [Running in the background](./user/background-service.md)
- [Updating Onicode](./user/updating.md)
- [Maintenance, power, and diagnostics](./user/diagnostics.md)
- [CLI reference (`onicode`)](./user/cli.md)
- [Triage an issue](./user/triage.md)
- Provider guides: [Codex](./user/providers-codex.md) · [Claude](./user/providers-claude.md) · [OpenCode](./user/providers-opencode.md) · [Antigravity](./user/providers-antigravity.md) · [Grok Build](./user/providers-grok.md) · [Kiro](./user/providers-kiro.md) · [Command Code](./user/providers-command-code.md) · [Pi](./user/providers-pi.md) · [Muse Code](./user/providers-muse.md) · [fx](./user/providers-fx.md)

---

## Working on Onicode

Start with the [development runbook](./operations/00_development.md) and
[contribution policy](../CONTRIBUTING.md).

Internal notes preserve architectural decisions, constraints, and implementation traps that the
source alone does not explain. Most code changes do not need an internal documentation update. Follow the
[documentation rules](../AGENTS.md#documentation) before adding one.

### Internals (in reading order)

- [00 · Glossary](./internals/00_glossary.md)
- [01 · Architecture](./internals/01_architecture.md)
- [02 · Effect services](./internals/02_effect-services.md)
- [03 · Providers](./internals/03_providers.md)
- [04 · Environment auth](./internals/04_environment-auth.md)
- [05 · Remote environments](./internals/05_remote.md)
- [07 · Server updates](./internals/07_server-updates.md)
- [09 · Connection runtime](./internals/09_connection-runtime.md)
- [10 · Web UI](./internals/10_web-ui.md)
- [11 · Mobile layout](./internals/11_mobile-layout.md)
- [12 · Mobile navigation](./internals/12_mobile-navigation.md)
- [13 · Mobile development lifecycle](./internals/13_mobile-development.md)
- [14 · Translating the UI (i18n)](./internals/14_i18n.md)
- [15 · Testing](./internals/15_testing.md)
- [16 · Performance regression checks](./internals/16_performance-regressions.md)
- [17 · Scripts](./internals/17_scripts.md)

Onicode only:

- [Push notifications](./internals/push-notifications.md)
- [Browser use](./internals/browser-use.md)
- [Computer use](./internals/computer-use.md)

### Runbooks

- [00 · Development and local builds](./operations/00_development.md)
- [01 · Dev container](./operations/01_devcontainer.md)
- [04 · Observability](./operations/04_observability.md)
- [Release](./operations/release.md)
- [Scenarios: the desktop end-to-end suite and real-app proof for a fix](./operations/scenarios.md)
- [Push gateway](./operations/push-gateway.md)
- [Upstream sync](./operations/upstream-sync.md)
