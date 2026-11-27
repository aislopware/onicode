# CLI reference (`onicode`)

The `onicode` command runs and manages Onicode servers, the background service, remote access, threads, workflows, and diagnostics. Run `onicode --help` or `onicode <command> --help` for every flag.

To install it, see [Install Onicode](./install.md#command-line):

```bash
curl -fsSL https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.ps1 | iex
```

## Running and connecting

| Command              | Description                                                                                                               |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `onicode [path]`     | Run a server in this terminal, add the directory as a project, and print pairing details for the apps.                    |
| `onicode serve`      | Run a server without adding a project (`--lan`, `--tailnet-only`, `--tailscale-serve`, `--host`, `--port`, `--base-dir`). |
| `onicode app [path]` | Open a directory as a project in the running desktop app.                                                                 |
| `onicode up`         | Set up remote access and the background service (`--lan`, `--no-lan`, `--tailnet-only`, `--yes`).                         |
| `onicode status`     | Show the service setup and how other devices can reach this host (`--json`).                                              |
| `onicode pair`       | Mint a one-time pairing link and QR code for a running server (`--tailscale`, `--scope`, `--ttl`, `--label`).             |

## Background service and updates

| Command                                 | Description                                                                                                                                                 |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `onicode service`                       | Manage the background service (`install`, `update`, `status`, `restart`, `uninstall`, `protect`). See [Running in the background](./background-service.md). |
| `onicode update [version]`              | Update `onicode` and the background service (`--allow-downgrade`, `--yes`). See [Updating Onicode](./updating.md).                                          |
| `onicode install` / `onicode uninstall` | Install this version as the `onicode` command, or remove the service, the launcher, and downloaded versions (your projects and threads are kept).           |

## Threads, projects, and workflows

| Command            | Description                                                                                                                                                                                                   |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `onicode thread`   | Script threads on the host (`start`, `send`, `show`, `list`; with `--wait`, `--json`, `--worktree`, `--provider`, `--model`, `--option`). See [Scripts on the host](./outside-agents.md#scripts-on-the-host). |
| `onicode project`  | Manage projects on the host (`add`, `remove`, `rename`; for example `onicode project add <path> --track <folder>`).                                                                                           |
| `onicode workflow` | Start and steer workflows (`start`, `list`, `show`, `resume`, `cancel`, `questions`, `signal`, `withdraw`, `hooks`, `store`). See [Workflows](./workflows.md).                                                |

## Environments, access, themes, and policy

| Command               | Description                                                                                                                                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `onicode environment` | Link other environments so agents here can work there (`link <address> --pairing-code <code> --access <mode>`, `link --from <file>`, `list`, `unlink`). See [Link another environment](./remote-access.md#link-another-environment). |
| `onicode auth`        | Manage pairing codes, sessions, and outside agents on a host without a desktop (`pairing create/list/revoke`, `session issue/list/revoke`, `agent token/code`). See [Outside agents](./outside-agents.md).                           |
| `onicode theme`       | Manage the theme this server publishes (`set <id>`, `clear`, `show`). See [Environment themes](./appearance.md#environment-themes).                                                                                                  |
| `onicode policy`      | Inspect and validate [managed policies](./managed-policy.md), and generate MDM and Group Policy templates (`show`, `check`, `settings`, `templates`).                                                                                |

## Diagnostics and maintenance

| Command                 | Description                                                                                                                        |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `onicode doctor`        | Inspect the data directory and repair it, such as reclaiming database space (`--fix`, `--dry-run`).                                |
| `onicode browser setup` | One-time setup of a Linux host for the browser: its sandbox, libraries, and fonts.                                                 |
| `onicode trace summary` | Summarize latency and errors from the server's trace files (`--since 30m`, `--limit 50`).                                          |
| `onicode triage`        | Investigate an Onicode problem with Claude Code, Codex, or Antigravity and help file an issue. See [Triage an issue](./triage.md). |
