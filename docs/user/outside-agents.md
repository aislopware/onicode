# Outside agents

Agents that Onicode did not start can work with an environment through its MCP
server. They can read projects and threads, start and message threads, and
check which providers and models are available. This covers local tools such as
Claude Code and Codex as well as cloud apps such as ChatGPT, Claude, and Poke.
Each app connects once, and you choose what it may do.

## Set up an app

Open **Use in another app (MCP)** from **Settings → Sharing & access →
Authorized clients** on desktop, choose **Use Onicode in another app (MCP)** in
the command palette, or open **Settings → Environments →** the machine **→
Other apps → Use in another app (MCP)** on mobile. Pick the app to see its MCP
URL and setup commands or steps.

- **Apps on your own computers** use the environment's address followed by
  `/mcp`. MCP clients only send sign-in credentials over `https` or to
  `localhost`, so a plain-`http` LAN or Tailscale IP address cannot finish
  signing in. Run the agent on the host using `localhost`, enable
  [Tailscale HTTPS](./remote-access.md#tailscale-https), or turn on
  [public agents](./remote-access.md#public-access).
- **Cloud apps** (such as ChatGPT, Claude, and Poke) reach your machine from the
  internet. They need [public agents](./remote-access.md#public-access) turned
  on and use `https://<machine>.<tailnet>.ts.net:8443/mcp`. The setup dialog can
  turn **Public agents** on if your connection may change settings.

To copy the MCP URL of the machine you are on without the dialog, use **MCP URL**
in **Settings → Sharing & access**. It shows the public address when public agents
are on, otherwise a `localhost` or `https` address.

## Approve a sign-in

The first time an app connects, it opens a sign-in page served by your Onicode
server. The page displays a match code and waits for your approval.

An approval dialog opens in Onicode on every connected device (desktop and
mobile) that may manage access on that environment. Check that the match codes
match, choose what the agent may do, then pick **Approve** or **Deny**. The
sign-in page then returns to the app.

The page and dialog warn when the callback address is not on the same computer;
anyone who runs that address receives the access. Approve only sign-ins you just
started.

**Supervised** is preselected, or the closest lower level your device and
organization policy allow (levels your device or policy cannot grant are
disabled):

- **Read only:** Read projects and threads. Cannot start, message or change
  anything.
- **Supervised:** Ask before commands and file changes.
- **Auto-accept edits:** Auto-approve edits, ask before other actions.
- **Auto:** Supported providers approve routine actions; others still ask.
- **Full access:** Allow commands and edits without prompts. Also allows
  changing projects and environment settings.

A thread an outside agent starts or steers cannot run with broader permissions
than the level you chose. Some agents have modes of their own that skip
approvals; only an agent with **Full access** can pick one.

### Approve with an agent code

If you do not have an Onicode app at hand, select **No app at hand? Enter an
agent code instead.** on the sign-in page.

Create a code from **Set up another way → No Onicode device at hand? Approve
with a code** in the setup dialog, or on the host with:

```sh
onicode auth agent code --access approval-required
```

`--access` accepts `read-only`, `approval-required`, `auto-accept-edits`,
`auto`, or `full-access` (defaults to `approval-required`). A code looks like
`SUPERVISED-XXXX…`, works once, expires in 5 minutes, and carries the access
level you chose. Five wrong codes end the sign-in.

## Claude Code

```sh
claude mcp add --transport http onicode <url>
claude mcp login onicode
```

`claude mcp login` opens the sign-in page in your browser. On a machine without
a browser, add `--no-browser`, open the printed URL elsewhere, and paste the
final URL back when asked. `claude mcp list` shows `onicode` as connected once
you approve.

## Codex

```sh
codex mcp add onicode --url <url>
codex mcp login onicode
```

`codex mcp login --no-browser` prints the sign-in URL instead of opening it.
`codex mcp list` shows `onicode` with OAuth once you approve.

## Other apps and static tokens

Any app that supports remote MCP servers over HTTP with OAuth can connect: give
it the MCP URL and choose OAuth, and it discovers the sign-in settings itself.

For an app that sends a fixed header instead of signing in, create a bearer
token from **Set up another way → App without sign-in? Create a token** in the
setup dialog, or on the host with:

```sh
onicode auth agent token --label "Nightly triage" --access approval-required
```

Pass `--json` for machine-readable output. Configure the app to send
`Authorization: Bearer <token>` to the MCP URL. Copy the token when you create
it; Onicode does not show it again.

## Events

Apps that support MCP events can subscribe to milestones instead of polling: a
run settling, a thread waiting for input and that input arriving, and a watched
pull request waking an agent. With workflows on, workflow runs report starting,
pausing, resuming, needing input and ending too.

An app can receive them as webhooks. Onicode sends each one signed with the
secret the app chose, after the address answers a signed challenge. Addresses on
your network or tailnet may use plain http; anything else needs https. Onicode
never delivers to this machine's own loopback or link-local addresses. Deliveries stop when
you revoke the app, and those an app set up through its public address stop when
you turn public agents off.

## Scripts on the host

On the machine that runs the environment, `onicode thread` uses the same tools
without a sign-in:

```sh
id=$(onicode thread start . "Find why the tests are slow")
onicode thread send "$id" "Now fix it" --wait
onicode thread show "$id"
onicode thread list
```

`.` means the project that contains the current folder; a project id also
works. A new thread uses the project's default model and permission mode, and
runs in the project folder unless you pass `--worktree <base-branch>`. `send`
never interrupts: a busy thread gets the message as its next turn. Pass `-` as
the message to read it from stdin.

`--wait` prints the agent's reply. It exits 1 when the run fails or stops, and
2 when the agent waits for an answer you give in the app. Add `--json` for a
result that always includes the thread id.

## Remove an agent

Sign-ins and static tokens last 30 days. Approved agents appear under
**Settings → Sharing & access → Authorized clients** alongside other clients
(and in `onicode auth session list`). On mobile they are under **Connected
agents** on the environment's **Use in another app (MCP)** page. Revoke one
there, or with
`onicode auth session revoke <id>`, to cut off its access immediately.

A [managed policy](./managed-policy.md#clients) whose `clients` leaves out
`remote` refuses outside agents.
