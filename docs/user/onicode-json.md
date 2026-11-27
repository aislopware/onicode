# Project file (`onicode.json`)

Check an `onicode.json` file into your repository root to share project actions (setup, dev, test, and cleanup scripts), worktree defaults, an icon, and multi-repository settings with everyone who opens the repository in Onicode.

## Inheritance order

The settings `onicode.json` can declare, the workspace for new threads and submodules, resolve in this order (the first value that is not **Inherit** wins):

1. **Project override** in Settings
2. **Environment setting** in Settings
3. **`onicode.json`** in the repository
4. **Built-in default**

## Example

```json
{
  "iconPath": "assets/logo.svg",
  "defaultThreadEnvMode": "worktree",
  "worktreeSubmodules": "top-level",
  "worktreeSparsePatternsPath": ".onicode/sparse-checkout",
  "ignoreRepositories": ["vendor/big"],
  "scripts": [
    {
      "name": "Setup",
      "command": "pnpm install",
      "icon": "configure",
      "runOnWorktreeCreate": true,
      "async": false
    },
    {
      "name": "Dev",
      "command": "pnpm dev",
      "icon": "play",
      "previewUrl": "http://localhost:3000",
      "autoOpenPreview": true,
      "stopOnSettle": true
    },
    {
      "name": "Clean",
      "command": "cargo clean",
      "icon": "build",
      "runOnSettle": true
    }
  ]
}
```

### Top-level fields

| Field                        | Values / Description                                                                                                                                                                |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `iconPath`                   | Workspace-relative path to a project icon, checked before the icons Onicode finds itself.                                                                                           |
| `defaultThreadEnvMode`       | `"local"` or `"worktree"`: where new threads start.                                                                                                                                 |
| `worktreeSubmodules`         | `"recursive"` (default), `"top-level"`, or `"none"`.                                                                                                                                |
| `worktreeSparsePatternsPath` | Repository-relative path to a Git sparse-checkout pattern file (non-cone syntax; Git 2.35 or newer). If the file is missing, creating the worktree fails.                           |
| `ignoreRepositories`         | Paths of repositories inside the project folder to leave out of a [multi-repository project](./multi-repo-projects.md).                                                             |
| `worktreeLayout`             | `{ "create": "...", "remove": "..." }` commands that lay out a worktree thread's folder yourself. Needs approval ([details](./multi-repo-projects.md#lay-the-folder-out-yourself)). |
| `scripts`                    | Up to 50 project actions (see below).                                                                                                                                               |

A new worktree reads `worktreeSubmodules` and `worktreeSparsePatternsPath` from the branch being checked out.

## Project actions (`scripts`)

Import `onicode.json` scripts, or define your own actions, in the project's **Actions** settings under **Settings → Projects**. Run them from the actions menu, the command palette, or a [keybinding](./keybindings.md) on `script.<id>.run`.

| Field                 | Description                                                                                                                              |
| --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `name`                | Name shown in the actions menu.                                                                                                          |
| `command`             | Shell command run in an Onicode terminal at the project root.                                                                            |
| `icon`                | `"play"` (default), `"test"`, `"lint"`, `"configure"`, `"build"`, or `"debug"`.                                                          |
| `runOnWorktreeCreate` | Runs after a worktree is created for a new thread. Skipped for worktrees an agent below **Full access** creates, until a person runs it. |
| `async`               | For `runOnWorktreeCreate` only: `true` (default) starts the agent right away; `false` waits for the script to finish first.              |
| `stopOnSettle`        | Stops this action's terminal and its child processes when its thread settles.                                                            |
| `runOnSettle`         | Runs in the thread's worktree each time the thread settles; threads without their own worktree skip it.                                  |
| `previewUrl`          | Opens this URL in the built-in browser when the script runs (desktop only).                                                              |
| `autoOpenPreview`     | Opens the browser panel at `previewUrl` as soon as the script starts.                                                                    |

In a project with several repositories, only the project folder's `onicode.json` is read; the files inside its repositories are ignored.
