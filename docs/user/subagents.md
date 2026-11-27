# Subagents and delegation

An agent can split work across several agents at once, either through subagents its provider starts itself or by delegating tasks through Onicode, so research, implementation, and reviews run in parallel on different models, worktrees, or machines.

## Two kinds of subagents

| Type                           | How it starts                                                                                                                                                         | Interactive?                                 | Where approvals go                             |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- | ---------------------------------------------- |
| **Onicode-delegated subagent** | The agent calls `delegate_task`, which can choose another provider or model, its own Git worktree, or a [linked machine](./remote-access.md#link-another-environment) | Yes, it has its own composer                 | In the child thread, and shown in the parent   |
| **Provider subagent**          | The provider starts it itself, as Claude and Codex can                                                                                                                | Read-only; message the parent thread instead | In the parent thread, with the subagent's name |

Type `@` and part of a model name, such as `@opus` or `@opus:opencode`, in your prompt to tell the agent which model to delegate to without changing the thread's own model.

## Follow and manage subagents

- **Inspect**: Click a subagent card in the conversation, or open **Agents** in the thread, to see its timeline.
- **Side panel or main view**: On desktop, a subagent opens as a tab in the right panel; `Cmd`/`Ctrl`-click or middle-click opens it in the main view. Change the default in **Settings → Chat → Open subagents and side chats in** ([details](./thread-sidebar.md#open-a-thread-beside-the-one-you-are-in)).
- **Stop and archive**: **Stop** on a parent thread also stops its delegated subagents. Wait for running delegated tasks, or cancel them, before archiving the parent.
- **Permissions**: A delegated subagent never gets a broader [permission mode](./permission-modes.md) than its parent thread.
- **Linked machines**: A task delegated to a linked machine runs as a regular thread on that machine and wakes the parent when it is done. You can follow it live, read-only, without pairing with that machine ([Read a linked machine's threads](./remote-access.md#read-a-linked-machines-threads)).
