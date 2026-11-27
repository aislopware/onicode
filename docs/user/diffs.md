# Diffs and workspace files

Inspect changes, read or edit workspace files, and send line-specific feedback to your agent without leaving the thread.

## Diff pane

Open the Diff pane in the right panel (desktop) and choose a source in its picker:

- **Latest turn** or a numbered turn: The files that turn edited, committed, or resolved conflicts in. Files that a `git pull`, merge, or rebase brought in are left out.
- **Uncommitted**: The current working-tree changes.
- **Changes**: The whole branch compared with its base branch.
- **Commits**: The branch's commits one by one, up to the newest 100.

In a [project with several repositories](./multi-repo-projects.md), the repository picker beside the source picker narrows the view to one repository.

To search, click into a diff and press `mod+f`. The search covers every loaded file and unfolds files that match; `Enter` and `Shift+Enter` move between matches.

## Workspace files

Open files from an agent's file links, the file picker, or the Diff pane to read or edit them beside the conversation. Editing over a remote connection needs the `filesystem:write` permission.

- Code and JSON are highlighted; Markdown, HTML, CSV, and TSV have rendered and raw views; PDFs preview, and you can browse ZIP archives.
- Files outside the workspace open read-only.

## Review comments

Select lines in a diff or a workspace file to leave a review comment. Comments attach to your composer as context chips, so you can collect feedback across several files and send it to the agent in one message.

## Diff settings

Set the defaults in **Settings → Diffs & pull requests**:

- **Diff layout**: **Stacked** or **Split** (side by side).
- **Diff colors**: Standard or colorblind-friendly.
- **Hide whitespace changes**: Ignore changes that only touch whitespace.
- **Default diff file state**: Files start expanded or collapsed.
- **Open pull requests and changes panels proactively**: Opens the relevant panel when a turn finishes.
- **Open pull requests in Onicode**: Opens pull request links inside Onicode instead of on your Git host.
