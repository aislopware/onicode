# Environment authentication

The environment issues its own sessions and enforces their capabilities. See
[SECURITY.md](../../SECURITY.md) for the overall threat model.

## Scoped sessions and delegation

- **Narrowing only.** Pairing delegates a set of scopes. Exchanging a bootstrap credential can
  narrow a grant, never widen it. Creating another pairing link requires both `access:write` and
  every scope being delegated. Client labels and device metadata have no authorization role.
- **`access:write` hands out what the session holds and revokes anything.** Every route that
  returns a credential to the caller requires `access:write` and every scope that credential
  carries: a pairing link and a link code from a linked environment.
  Revoking is not capped by the caller's scopes.
- **Why the default grant includes access management.** `terminal:operate` and
  `orchestration:operate` already grant OS-user execution (which can invoke `onicode auth session issue`),
  so withholding access management from the default grant would not be a real boundary. Narrower
  clients receive explicit scopes.
- **No recoverable secrets in read models.** Only the creation response returns a raw pairing
  credential; access listings expose metadata only.
- **Bearer sessions.** Every session is a bearer token. A [peer link](./05_remote.md#peer-links)'s
  session renews on use: its token is never reissued, so the session row alone holds the deadline,
  moved 30 days out at most once a day. Only an unexpired, unrevoked row moves, so an idle link
  still ends and a revoke is final. Outside-agent and thread CLI sessions never renew.
- **Desktop bootstrap replaces earlier sessions.** Desktop restarts forget the previous local
  token, so its reusable bootstrap grant revokes earlier sessions for the same subject in the same
  [transaction](../../apps/server/src/persistence/AuthSessions.ts) that inserts the new one; a failed
  replacement leaves the old credential usable. Pairing sessions do not follow this rule.
- **WebSocket tickets and per-RPC authorization.** Clients mint short-lived tickets over
  authenticated HTTP so long-lived tokens never appear in socket URLs. A ticket names the route it
  opens: the RPC socket, or the media routes (device hub, preview stream), which share one ticket.
  Clients that predate route-named tickets mint ones that open either route, so the server keeps
  accepting those.
  A successful handshake grants no extra authority: every RPC declares and checks its required
  scope in [`RpcAuthorization.ts`](../../apps/server/src/auth/RpcAuthorization.ts).
- **Secret references need `providers:manage`.** A request that names a secret for the server to
  read needs `providers:manage` on top of its method's scope, whether the reference is saved in
  settings or used once, as in a secret request answered with a reference. Typed values need only
  the method's scope. Clients hide the secret provider picker from sessions without it.
- **Project scripts need `orchestration:operate`.** The host runs a saved project script itself,
  on worktree creation and on settle, so saving one needs `orchestration:operate` on top of
  `settings:write`, whether it arrives in a settings patch or a project update.
- **Backward-compatible scope evolution.** Token exchange intersects requested scopes with the
  pairing grant, dropping retired/unknown names and failing before consuming a link if none remain.
  Stored grants are never expanded when scopes split. Auth responses preserve legacy `scopes`
  alongside exact `permissions`, and permission errors keep a legacy `requiredScope` beside the
  exact `requiredPermission`, so older clients and servers stay compatible. Unknown response
  permissions are ignored; grant inputs stay strict.

## Non-standard principals

- **`/mcp` admits outside agents, peer links, and `onicode thread`.** Outside agents (subject
  `outside-agent`) and peer links (`mcp-client`, carrying a signed `peerLink` claim) sign in
  through [`McpOAuth`](../../apps/server/src/auth/McpOAuth.ts); outside agents may also use static
  bearer tokens. Client registration is stateless (HMAC-signed `client_id`) so unauthenticated
  callers cannot grow server state. Legacy `mcp-client` sessions without the signed `peerLink`
  claim stay rejected. `onicode thread` gets a short-lived `thread-cli` session from the running
  server. On public requests ([`IsPublicRequest`](../../apps/server/src/publicAccess/publicRequest.ts)),
  [`McpHttpServer`](../../apps/server/src/mcp/McpHttpServer.ts) skips internal provider thread
  tokens so only signed-in outside-agent credentials work over Funnel.
- **Grants are read-only or a runtime-mode ceiling, not raw scopes.** Every MCP tool must statically
  declare its access level in [`McpToolAccess.ts`](../../apps/server/src/mcp/McpToolAccess.ts).
  Every other HTTP and WebSocket route rejects `outside-agent`, `mcp-client`, and `thread-cli`
  subjects, except the read-only thread routes in
  [`linkThreadHttp`](../../apps/server/src/peer/linkThreadHttp.ts) and client pairing, both
  restricted to peer links. Inside MCP the credential sets the limits and tool parameters only pick
  targets ([`threadAccess`](../../apps/server/src/mcp/threadAccess.ts)).
- **In-app approval and static agent tokens cannot widen authority.** Answering a pending sign-in
  or issuing a static agent token requires `access:write`, intersects the requested access level
  with the approving session's scopes, and checks managed policy server-side in
  [`McpOAuth`](../../apps/server/src/auth/McpOAuth.ts). Single-use agent approval codes
  (`mcp-approval:agent:<level>`) are purpose-bound, hidden from normal pairing listings, and
  enforce the same narrowing and policy checks at mint time.
- **The OAuth consent page is the only browser page the server serves.**
  [`mcpOAuthHtml.ts`](../../apps/server/src/auth/mcpOAuthHtml.ts) serves static HTML with
  `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'`,
  no scripts, and `<meta http-equiv="refresh">` polling while waiting for in-app approval. Code
  submissions redirect back to the consent page before redirecting off-origin to the client's
  callback.
- **Thread-only tools refuse outside callers in handlers.** Tools that act as a calling thread
  (`request_secret`, `delegate_task`) are hidden from these callers, but hiding is only discovery:
  handlers still refuse with `thread_credential_required`.
- **Only the broadest mode drives workflows.** A workflow run picks its own modes, so only a caller
  at the broadest mode a thread may run in here (full access, or an administrator's ceiling below
  it) can start or change runs, change what workflows store, or read webhook paths. Narrower
  callers can still read runs.
- **Peer-link client pairing.** Only a full-access peer link may mint a one-time client pairing on
  the linked environment (its agents can already run `onicode auth pairing create`). The linked side
  enforces this itself, and the operation stays a human-triggered RPC/HTTP route rather than an MCP
  tool. The same route mints a code that links another environment at a chosen access level, and
  the linking side's RPC requires the caller to hold that level's scopes.
- **The issuer comes from the request.** The issuer URL uses `Host` and `X-Forwarded-Proto`, so one
  server answers over loopback, the LAN, Tailscale Serve, and Tailscale Funnel (where the public
  listener rewrites both headers to the Funnel origin). A proxy that rewrites `Host` or drops the
  protocol header breaks OAuth.
- **Tailnet identity counts only on a direct tailnet socket.**
  [TailnetAccess](../../apps/server/src/tailnet/TailnetAccess.ts) signs a device in by asking
  `tailscale whois` about the socket peer, trusted only when both ends are tailnet addresses.
  Loopback (`tailscale serve`, userspace tailscaled, local processes) is refused. Never take the
  peer from `X-Forwarded-For` or another header. A tagged host admits only devices an ACL grant
  gives `onicode.com/cap/environment`; sessions use subject `tailnet:<nodeId>` and are revoked with
  the other remote sessions when a managed policy keeps clients local.

## The environment is the filesystem boundary

Projects are organizational boundaries, not filesystem sandboxes. `filesystem:read` permits reading
any file the server's OS user can read (for example, artifacts an agent writes in a temp directory);
relative paths and writes remain constrained by
[`WorkspaceFileSystem.ts`](../../apps/server/src/workspace/WorkspaceFileSystem.ts).

Signed asset URLs are bearer credentials for a single canonical file, rechecked by device/inode
identity on serve ([`AssetAccess.ts`](../../apps/server/src/assets/AssetAccess.ts)). They never
grant access to sibling files, and because `filesystem:read` is checked only at mint time, an issued
URL remains valid until it expires even if the session or grant is revoked.

Host videos can change in place, so their [HTTP responses](../../apps/server/src/http.ts) omit cache
validators: file metadata cannot prove byte identity for `If-Range`, and weak validators would turn
native-player seeks into full downloads.
