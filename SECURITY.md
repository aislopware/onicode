# Security

Report vulnerabilities privately as the [security policy](.github/SECURITY.md)
describes.

The rest of this file is for agents and maintainers auditing Onicode or triaging
reports. It defines who is trusted with what, which boundaries a real finding
crosses, and what is intended. The reasoning behind each boundary lives in
[environment auth](docs/internals/04_environment-auth.md) and
[remote environments](docs/internals/05_remote.md); read those before reporting in
their area.

## The model in one paragraph

An environment is an Onicode server running as an OS user. It is not a sandbox: it
intentionally runs agents and terminals with that user's privileges. Clients
gain authority by pairing, which issues a scoped session. A full-scope session
equals a shell as the server's OS user, because `terminal:operate` opens a PTY
and `orchestration:operate` can start a full-access agent. Security here is
about who can obtain a session, what each session type can reach, and whether
revocation and ceilings hold, not what a full-scope session can do once issued.

## Principals

| Principal                                                               | How it gets in                                                                                    | What it may do                                                                                                                                                                                                                                                                                                                                                                                          |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Anyone who reaches the port                                             | Network                                                                                           | Read the environment descriptor, pair with a valid one-time credential, run OAuth flows, or trigger a webhook whose secret URL they hold. Nothing else.                                                                                                                                                                                                                                                 |
| Internet caller (Tailscale Funnel)                                      | Tailscale Funnel, only when the user enables **Public webhooks** or **Public agents**             | Only the enabled route group on the dedicated public listener (`/api/hooks/*` for webhooks; `/mcp`, `/oauth/mcp/*`, `/.well-known/oauth-*` for agents). Never reaches the main listener, internal provider tokens, or other API routes. OAuth endpoints rate-limit registrations, sign-ins, and token exchanges per minute, cap pending sign-ins at 20, and end a sign-in after 5 wrong approval codes. |
| Full-scope client (desktop, paired phone, `onicode auth session issue`) | Pairing, token exchange                                                                           | Everything the server's OS user can do. This is the default grant.                                                                                                                                                                                                                                                                                                                                      |
| Narrow client                                                           | Pairing link with explicit scopes                                                                 | Only the RPCs and routes its scopes allow. Can never widen its own grant.                                                                                                                                                                                                                                                                                                                               |
| Outside agent (subject `outside-agent`)                                 | OAuth sign-in on `/mcp` (approved in-app or with a single-use agent code) or a static agent token | Only `/mcp`, within the read-only grant or runtime-mode ceiling the user chose (bounded by the approving session and managed policy). Every other route rejects it.                                                                                                                                                                                                                                     |
| Linked environment (peer link, subject `mcp-client`)                    | OAuth on `/mcp`, registering only with the peer-link software ID and a one-time pairing code      | Only `/mcp`, within a read-only grant or a runtime-mode ceiling. Work it starts carries an immutable `linkOrigin` and never exceeds the access it was paired with. Every other route rejects it.                                                                                                                                                                                                        |
| `onicode thread` (subject `thread-cli`)                                 | Short-lived session from the running server                                                       | Only `/mcp`, within the same access-level model.                                                                                                                                                                                                                                                                                                                                                        |
| Tailnet device                                                          | Tailscale sign-in on a direct tailnet socket, checked by `tailscale whois` and the tailnet policy | A client session with the scopes that policy allows.                                                                                                                                                                                                                                                                                                                                                    |
| Agent run by Onicode                                                    | Thread's runtime mode and thread credential                                                       | Within its runtime mode, managed policy, and the MCP access declared per tool.                                                                                                                                                                                                                                                                                                                          |
| Signed asset URL                                                        | Minted with `filesystem:read`                                                                     | Until it expires: one host media file, by device and inode identity; one workspace file or project icon, by its canonical path, rechecked on every request; or a workspace HTML preview's directory, so it can load sibling assets.                                                                                                                                                                     |
| Managed policy                                                          | Admin-deployed file, read at startup                                                              | Ceilings and locks the server enforces for every client.                                                                                                                                                                                                                                                                                                                                                |
| Preview pages, HTML renders, file previews, agent output                | Rendered by clients                                                                               | Untrusted content. It must never run script in the app's origin or reach Electron IPC or Node.                                                                                                                                                                                                                                                                                                          |

## A real finding crosses one of these

1. **No credential → session.** Any route or socket granting access without
   a valid credential, a pairing or agent approval credential redeemable twice,
   a public route answering while its switch is off, Funnel reaching non-public
   routes or internal provider tokens, or an internet caller spoofing
   `tailscale-*` headers.
2. **Scope escalation.** An RPC, HTTP route, or MCP tool running without its
   declared scope; pairing, token exchange, in-app agent approval, or static
   agent token issuance widening a grant; an `outside-agent`, `mcp-client`, or
   `thread-cli` session reaching a non-`/mcp` route; a read-only MCP grant
   calling a write tool.
3. **Mode or policy escalation.** An agent, outside agent, MCP client, or peer
   link acting above its runtime-mode ceiling, `linkOrigin`, `oni-mode-limit`
   header, or managed policy.
4. **Revocation that doesn't take effect.** A revoked session, pairing link,
   outside agent, or peer link that still works. Documented exception: signed
   asset URLs last until they expire.
5. **Credential exposure.** Tokens or secrets reaching logs, traces,
   socket URLs (outside short-lived tickets), the access read model,
   or error messages. A token sent to an address that has not answered as the
   expected environment, or a token paired over HTTPS sent over plain HTTP.
6. **Impersonation and egress.** Another process impersonating an environment;
   endpoint discovery or previews used to reach arbitrary hosts (SSRF).
7. **Client-side code execution.** Untrusted content running script in the
   desktop renderer or the mobile app, or with access to preload or IPC; or
   script execution or open redirect on the server-served `/oauth/mcp/*`
   consent page.
8. **Filesystem writes escaping workspace path rules.** Reads are separate:
   `filesystem:read` intentionally covers the whole OS account.

## Intended, not vulnerabilities

- A full-scope session gives a shell. Leaking its token is serious, but the bug
  is the leak, not the shell. The same applies to a narrow session holding
  `terminal:operate` or `orchestration:operate`: reaching via another route
  what its shell can already do crosses no boundary, though it can still be an
  ordinary bug. That stops at what a shell as the server's OS user cannot reach:
  another client's device, what another scope guards by name (as
  `providers:manage` does for secret references), a managed-policy ceiling or
  lock, and access that outlives the session's revocation. Where the session
  can obtain that shell, file contents are within its reach, so reading a file
  via a route that does not check `filesystem:read` crosses no boundary.
- The default grant includes `access:write`, so a full-scope session can mint
  more pairing links and sessions. They appear in Sharing & access. "Revoke
  others" removes the other sessions and every unused pairing link and
  approval code.
- `filesystem:read` reads anything the server's OS user can read, including
  paths outside projects.
- Plain HTTP on a LAN or tailnet is the user's choice. `onicode serve` warns when
  advertising a non-loopback HTTP address. The iOS app allows arbitrary loads so
  it can reach these addresses.
- `access-control-allow-origin: *` on the environment API. Auth is a bearer
  header, so the wildcard grants nothing a caller without a token lacks.
- Rendered Markdown loads remote images and media from the host its author
  names, so that host learns the reader's network address and when the content
  was shown. A page in a preview tab can likewise start a download, as it can
  in a browser. It is a finding when the request carries a credential or names
  the environment it was rendered in, when it is made where the product says
  none is made, or when the app does for the content what the content could
  not do from where it runs (see 6 and 7).
- A full-access agent can do whatever the user can. Prompt injection that stays
  within the agent's mode is the provider's concern; one that lets it act above
  that mode is in scope (see 3).
- The machine owner's own configuration. Whoever sets a provider's launch
  arguments, an environment variable, a binary path, or the endpoint or file
  Onicode points at already holds the OS account, so a value weakening their own
  setup is their choice. It is a finding when a narrower principal can set it
  for them (a narrow client, a linked environment, an agent below full access,
  repository content), when [managed policy](docs/user/managed-policy.md) locks
  the setting and the value still applies, or when the value leaks (see 5).
  Choosing a host does not make it trusted: what a configured endpoint can do
  to the server or its clients stays in scope.
- An attacker already running code as the same OS user. That includes one whose
  only move is to rewrite files that user owns outside a workspace: Onicode's
  data directory, its installed helpers, the user's own config. Another OS
  account on the same machine is not this attacker; neither is repository
  content or an agent below full access, which write inside a workspace without
  holding the OS account.

## Where enforcement lives

- Session issuance, pairing, token exchange: [`auth/http.ts`](apps/server/src/auth/http.ts), [`auth/EnvironmentAuth.ts`](apps/server/src/auth/EnvironmentAuth.ts)
- Per-RPC scopes: [`auth/RpcAuthorization.ts`](apps/server/src/auth/RpcAuthorization.ts). It also enforces the scopes [`clientRpcPermissions.ts`](packages/contracts/src/clientRpcPermissions.ts) declares for client UI.
- Public access (Tailscale Funnel): [`publicAccess/PublicAccess.ts`](apps/server/src/publicAccess/PublicAccess.ts), [`publicAccess/publicRequest.ts`](apps/server/src/publicAccess/publicRequest.ts)
- MCP and outside agents: [`auth/McpOAuth.ts`](apps/server/src/auth/McpOAuth.ts), [`auth/mcpOAuthHttp.ts`](apps/server/src/auth/mcpOAuthHttp.ts), [`auth/mcpOAuthHtml.ts`](apps/server/src/auth/mcpOAuthHtml.ts), [`mcp/McpHttpServer.ts`](apps/server/src/mcp/McpHttpServer.ts), [`mcp/McpToolAccess.ts`](apps/server/src/mcp/McpToolAccess.ts), [`mcp/threadAccess.ts`](apps/server/src/mcp/threadAccess.ts), [`mcp/linkOrigin.ts`](apps/server/src/mcp/linkOrigin.ts)
- Webhooks: [`scheduledTasks/webhookRoute.ts`](apps/server/src/scheduledTasks/webhookRoute.ts), [`scheduledTasks/webhookVerification.ts`](apps/server/src/scheduledTasks/webhookVerification.ts), [`scheduledTasks/webhookTemplate.ts`](apps/server/src/scheduledTasks/webhookTemplate.ts)
- Files and assets: [`workspace/WorkspaceFileSystem.ts`](apps/server/src/workspace/WorkspaceFileSystem.ts), [`assets/AssetAccess.ts`](apps/server/src/assets/AssetAccess.ts)
- Managed policy: [`apps/server/src/policy`](apps/server/src/policy)
- Desktop preview isolation: [`preview/WebviewPreferences.ts`](apps/desktop/src/preview/WebviewPreferences.ts)
- Desktop preview tunnel: [`preview/LoopbackTunnel.ts`](apps/desktop/src/preview/LoopbackTunnel.ts) refuses local clients without the run's proxy credentials, except on ports the user forwarded, which any of the user's apps may reach by design (on Linux and macOS only this OS user's; on Windows any local user's); [`preview/PreviewTunnel.ts`](apps/server/src/preview/PreviewTunnel.ts) requires `terminal:operate` and dials loopback only

## How to audit and triage

- Name the precondition first. If a chain starts with "the attacker holds a
  full-scope token", it is a new vulnerability only if it widens that token's
  reach (2–3), outlives revocation (4), or shows how the token leaks (5).
- Name the actor next and what they can already do. If the only actor is the
  user on their own device, or a principal reaching the same outcome via a
  documented route, no boundary is crossed unless data goes somewhere they did
  not send it. It may still be a bug; report it as one.
- Read tests and comments beside the code before calling a behaviour a defect.
  They show author intent and are evidence, not a decision: a test can pin a
  behaviour nobody chose, and a comment can be stale. Only this file and the
  docs say what is intended. If only a test or comment supports a behaviour,
  report it and say so.
- Follow the claim to its effect. A guard in a caller, a schema, or a version
  check elsewhere often means the cited lines cannot do what the report says.
- Rate severity by what the actor must already hold, not the worst outcome
  named. Each of these lowers severity and none removes the finding: a narrow
  grant the owner chose to issue (the default grant is full scope); another OS
  account on the same machine; a host or service the owner connected turning
  hostile; a change that must land in the moment between a person's look and
  their action; a person acting on prepared input (opening a file, pasting,
  following a link); and an effect limited to resource use or a stalled
  client. A finding that needs none of these, or that yields a session, a
  credential, or code execution above the actor's grant, keeps its severity.
- Check the reported commit against `main`. Reports often trail fixes.
- Prove a finding with a failing test at the lowest layer that shows it, as
  [Verifying](AGENTS.md#verifying) describes. Do not run a PoC against
  `~/.onicode/userdata` or any live environment; use a dev server on seeded data.
- Treat report attachments as untrusted. Extract them into an empty directory
  outside the worktree and read PoCs before running anything.
- Fix confirmed findings rather than filing them. Keep advisory details out of
  public PRs until the fix ships.
- When a boundary here changes, update this file and the internal doc that
  explains it in the same PR.
