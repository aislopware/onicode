# Command Code

Onicode runs [Command Code](https://commandcode.ai) as an agent through its ACP mode, using your
own Command Code account, settings, and MCP servers.

## Set Up Command Code

1. Install Command Code on the machine running the Onicode server with Node.js 22 or newer:
   `npm i -g command-code`.
2. Run `command-code login` once in a terminal, or set an API key as `COMMAND_CODE_API_KEY` in the
   Command Code provider's environment variables.
3. Open **Settings → Providers**, enable Command Code, and refresh it.

If `command-code` is not on the server's `PATH`, set its **Binary path**.

## Models

Command Code lists its models only inside a running session, so a new install shows just
**Command Code default**, the default model in your Command Code settings. After your first thread
runs, the model picker lists every model your account can use. A model's reasoning effort setting
appears once a thread has run on that model, with only the levels Command Code offers for it.

## Permission Modes

- **Supervised** runs Command Code's Standard mode: Onicode asks you before edits and commands.
- **Auto-accept edits** runs its Accept Edits mode: file edits apply on their own and commands still
  ask.
- **Full access** runs its bypass mode, which approves everything.

Command Code offers no **Auto**. A thread set to it runs in **Supervised**. **Plan** in the composer
runs Command Code's plan mode.

## Compaction

`/compact` runs Command Code's own compaction.
