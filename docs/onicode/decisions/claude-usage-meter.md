# Claude's usage meter comes from its turns

monocode drew Claude's footer meter by reading Claude Code's credentials and calling a private usage endpoint.
Onicode never reads another tool's credentials, and Claude Code publishes no usage poll. What it does publish is
`rate_limit_event` in its stream-json output: a `status` (allowed, allowed_warning, rejected), `resetsAt`, and
sometimes `utilization` and `rateLimitType`. Core keeps the newest report per window for each (provider, account) and
hands it to the same `Snapshot` the Codex poll fills, so monocode's chip, popover and account status render it
unchanged. A window without `utilization` shows its status word where the percentage would be.

The meter is therefore as fresh as the account's last turn ("Updated 4m ago"), and its refresh button stays dimmed.
Before the first turn the account reads as not yet known, never as signed out; the sign-in panel appears only after
the stream reports `authentication_failed`. Removing a Claude account runs the published `claude auth logout` for
its profile first, so the prompt's promise that its stored credentials go holds without Onicode touching them.

The reports live in the process that runs the turns. When turns move into the engine, the engine owns this ledger and
serves it to every client with the rest of the session state.
