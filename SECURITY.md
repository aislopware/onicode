# Security

Report vulnerabilities privately as the [security policy](.github/SECURITY.md)
describes.

The rest of this file is for agents and maintainers auditing Onicode or triaging
a report. It says who is trusted with what, which boundaries a real finding
crosses, and what is intended. The reasoning behind each boundary lives in
[environment auth](docs/internals/environment-auth.md) and
[remote environments](docs/internals/remote.md). Read those before you report a
finding in their area.

## The model in one paragraph

An environment is an Onicode server running as an OS user. It is not a sandbox: it
runs agents and terminals with that user's privileges, on purpose. Clients get
authority by pairing, which issues a scoped session. A full-scope session is
equivalent to a shell as the server's user, because `terminal:operate` opens a
PTY and `orchestration:operate` can start a full-access agent. Security work
here is about who can get a session, what each kind of session can reach, and
whether revocation and ceilings hold. It is not about what a full session can do
once it has one.

## Principals

| Principal                                                               | How it gets in                                                                                    | What it may do                                                                                                                                                                                   |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Anyone who reaches the port                                             | Network                                                                                           | Read the environment descriptor, pair with a valid one-time credential, run OAuth flows. Nothing else.                                                                                           |
| Full-scope client (desktop, paired phone, `onicode auth session issue`) | Pairing, token exchange                                                                           | Everything the server's OS user can do. This is the default grant.                                                                                                                               |
| Narrow client                                                           | Pairing link with explicit scopes                                                                 | Only the RPCs and routes its scopes allow. Can never widen its own grant.                                                                                                                        |
| Linked environment (peer link, subject `mcp-client`)                    | OAuth on `/mcp`, registering only with the peer-link software ID and a one-time pairing code      | Only `/mcp`, within a read-only grant or a runtime-mode ceiling. Work it starts carries an immutable `linkOrigin` and never exceeds the access it was paired with. Every other route rejects it. |
| `onicode thread` (subject `thread-cli`)                                 | Short-lived session from the running server                                                       | Only `/mcp`, within the same grants as a linked environment.                                                                                                                                     |
| Tailnet device                                                          | Tailscale sign-in on a direct tailnet socket, checked by `tailscale whois` and the tailnet policy | A client session with the scopes that policy allows.                                                                                                                                             |
| Agent run by Onicode                                                    | Thread's runtime mode and thread credential                                                       | Within its runtime mode, managed policy, and the MCP access declared per tool.                                                                                                                   |
| Signed asset URL                                                        | Minted with `filesystem:read`                                                                     | One file, by device and inode identity, until it expires. A workspace HTML preview gets its directory instead, so it can load sibling assets.                                                    |
| Managed policy                                                          | Admin-deployed file, read at startup                                                              | Ceilings and locks the server enforces for every client.                                                                                                                                         |
| Preview pages, HTML renders, file previews, agent output                | Rendered by clients                                                                               | Untrusted content. It must never run script in the app's origin or reach Electron IPC or Node.                                                                                                   |

## A real finding crosses one of these

1. **No credential → session.** Any route or socket that grants access without
   a valid credential, or a pairing credential that can be redeemed twice.
2. **Scope escalation.** An RPC, HTTP route, or MCP tool that runs without the
   scope it declares; pairing or token exchange that widens a grant; an
   `mcp-client` session reaching a non-`/mcp` route; a read-only MCP grant
   calling a write tool.
3. **Mode or policy escalation.** An agent, MCP client, or peer link acting
   above its runtime-mode ceiling, `linkOrigin`, `oni-mode-limit` header, or managed
   policy.
4. **Revocation that doesn't take effect.** A revoked session, pairing link,
   or peer link that still works. The exception is documented:
   signed asset URLs last until they expire.
5. **Credential exposure.** Tokens or secrets reaching logs, traces,
   socket URLs (outside short-lived tickets), the access read model,
   or error messages. A token sent to an address that has not answered as the
   expected environment. A token paired over HTTPS starting to travel over plain
   HTTP.
6. **Impersonation and egress.** Another process impersonating an environment,
   or endpoint discovery or previews used to reach arbitrary hosts (SSRF).
7. **Client-side code execution.** Untrusted content running script in the
   desktop renderer or the mobile app, or with access to preload or IPC.
8. **Filesystem writes escaping workspace path rules.** Reads are another
   matter: `filesystem:read` is intentionally the whole account.

## Intended, not vulnerabilities

- A full-scope token gives a shell. Leaking one is serious, but the bug is the
  leak, not the shell.
- The default grant includes `access:write`, so a full session can mint more
  pairing links and sessions. They appear in Sharing & access. "Revoke
  others" removes the other sessions; revoke unused pairing links one by one.
- `filesystem:read` reads anything the server's user can read, including paths
  outside projects.
- Plain HTTP on a LAN or tailnet is the user's choice. `onicode serve` warns when it
  advertises a non-loopback HTTP address. The iOS app allows arbitrary loads so
  it can reach these addresses.
- `access-control-allow-origin: *` on the environment API. Auth is a bearer
  header, so the wildcard grants nothing a caller without a token lacks.
- An agent in full-access mode can do whatever the user can. Prompt injection
  that stays within the agent's mode is the provider's concern. One that lets
  it act above that mode is in scope (see 3).
- An attacker already running code as the same OS user.

## Where enforcement lives

- Session issuance, pairing, token exchange: [`auth/http.ts`](apps/server/src/auth/http.ts), [`auth/EnvironmentAuth.ts`](apps/server/src/auth/EnvironmentAuth.ts)
- Per-RPC scopes: [`auth/RpcAuthorization.ts`](apps/server/src/auth/RpcAuthorization.ts). [`clientRpcPermissions.ts`](packages/contracts/src/clientRpcPermissions.ts) only drives client UI and is never the boundary.
- MCP: [`auth/McpOAuth.ts`](apps/server/src/auth/McpOAuth.ts), [`mcp/McpToolAccess.ts`](apps/server/src/mcp/McpToolAccess.ts), [`mcp/threadAccess.ts`](apps/server/src/mcp/threadAccess.ts), [`mcp/linkOrigin.ts`](apps/server/src/mcp/linkOrigin.ts)
- Files and assets: [`workspace/WorkspaceFileSystem.ts`](apps/server/src/workspace/WorkspaceFileSystem.ts), [`assets/AssetAccess.ts`](apps/server/src/assets/AssetAccess.ts)
- Managed policy: [`apps/server/src/policy`](apps/server/src/policy)
- Desktop preview isolation: [`preview/WebviewPreferences.ts`](apps/desktop/src/preview/WebviewPreferences.ts)
- Desktop preview tunnel: [`preview/LoopbackTunnel.ts`](apps/desktop/src/preview/LoopbackTunnel.ts) refuses local clients without the run's proxy credentials; [`preview/PreviewTunnel.ts`](apps/server/src/preview/PreviewTunnel.ts) requires `terminal:operate` and dials loopback only

## How to audit and triage

- Name the precondition first. If a chain starts with "the attacker holds a
  full-scope token", it is a new vulnerability only if it widens that token's
  reach (2–3), outlives revocation (4), or shows how the token leaks (5).
- Check the reported commit against `main`. Reports often trail fixes.
- Prove a finding with a failing test at the lowest layer that shows it, as
  [Verifying](AGENTS.md#verifying) describes. Do not run a PoC against
  `~/.onicode/userdata` or any live environment. Use a dev server on seeded data.
- Treat report attachments as untrusted. Extract them into an empty directory
  outside the worktree and read PoCs before running anything.
- Fix confirmed findings rather than filing them. Keep advisory details out of
  public PRs until the fix ships.
- When a boundary here changes, update this file and the internal doc that
  explains it in the same PR.
