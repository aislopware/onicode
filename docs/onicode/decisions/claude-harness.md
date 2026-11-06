# Claude harness: the CLI's own signals end turns, and everything else rides its published doors

Onicode drives Claude Code only through the stream-json print mode and its control channel, with
`CLAUDE_CODE_ENTRYPOINT=onicode`. It never sends requests in Claude Code's name, never reads its credentials and
never impersonates the SDK or desktop entrypoints. Where the CLI already says what happened, onicode reads that
instead of guessing from quiet time.

Turn ends come from the CLI. The run sets `CLAUDE_CODE_SDK_READS_SESSION_STATE=1`. Once a
`session_state_changed` arrives, the run trusts three signals:

- `idle`: the turn is over.
- `result.user_message_uuids`: which of our messages the turn consumed.
- `queued_turn_count`: more turns are queued behind this one.

A result is a boundary rather than the end while one of our messages is still waiting, more turns are queued
or a `turn_preempted` came first. It is released when the waiting message starts its own turn, when every
waiting message is cancelled, or at `idle`. The 3, 5 and 30 second settle timers apply only to a CLI that
never reports its state. The one timer left in the new regime is a safety net for a line the CLI never
acknowledged reading.

A stop settles in one of three ways:

- from the interrupt receipt, whose `cancelled` messages stop waiting;
- with the stopped turn's result;
- at `idle`, and at once if the CLI was already idle.

A late result that consumed only messages open at the stop is dropped. `rapidFollowupPreempt` is declared on
CLIs new enough to have it; its `turn_preempted` path is built from the protocol and still needs checking
against a live CLI.

Prompts and steers go out under the host's message id when it is a canonical uuid the session has not seen.
That lets `rewind_files` and branches name them. Anything else gets a fresh uuid, mapped back. Typed lines
carry `origin: human`, and a revival's continuation carries none. Messages that agents send through
onicode's MCP tools arrive through the same doors and are stamped as human too: the queue has no author field
yet.

Approvals offer five answers:

- allow once;
- allow for this chat: the CLI's `permission_suggestions` with the destination set to `session`;
- always allow: the suggestions as given;
- deny, with any typed text as the note;
- stop: deny with `interrupt`.

The lasting options are shown only when the CLI offers a rule and does not suppress it. The card also shows
the decision reason (ANSI stripped) and the blocked path. `default_to_no` and `requires_user_interaction`
travel as `ApprovalContext`: deny comes first, and the composer takes no one-key answer. A
`control_cancel_request` aborts the pending ask, which the engine reports as resolved unanswered.

MCP `elicitation` forms use the bridge Codex already has. URL elicitations ask the person to open the link
and say Done. `supportedDialogKinds` is declared empty, so `request_user_dialog` is never answered.
`CLAUDE_CODE_STARTUP_FAILURE_RESULTS=1` turns startup failures into results, mapped to plain words.

Fresh runs pass `--session-id`. A resume is refused while another live process holds the session; this
check reads only pids and session ids from `~/.claude/sessions/*.json`. File checkpoints are on. Branches
resolve against the session's transcript into `--resume-session-at`, `--resume-drops-turn` and
`--fork-session`. Flags are gated on the binary's `--version`, and an unknown version counts as current.
Startup waits 60 s for the `initialize` answer. It waits longer while a SessionStart or Setup hook reported by
`--include-hook-events` runs, up to 10 minutes.

If the engine dies, its agents die too. Each agent's process group is registered with one `/bin/sh` watcher
per engine over a pipe. When the engine exits for any reason, the pipe reaches EOF and the watcher sends
SIGTERM, then SIGKILL 3 s later. This replaces a kqueue watch on the engine's pid, because EOF cannot be fooled
by pid reuse.

Still to build:

- the engine RPC and UI for edit-and-resend and fork, which need doc truncation and branching;
- a read-only tail of a session another process holds;
- a context-breakdown panel over `get_context_usage`;
- a task-progress summary;
- retracting text a `tombstone` withdraws;
- an author field so agent-sent messages stop reading as human.
