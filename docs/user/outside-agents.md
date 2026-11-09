# Outside agents

Agents that Onicode did not start can work with an environment through its MCP
server. They can read projects and threads, start and message threads, and
check which providers and models are available. This covers Claude Code or
Codex in your own terminal and bots that support MCP. Each agent signs
in once, and you choose what it may do.

## Get the MCP URL

In **Settings → Environments**, open a saved environment's menu and choose
**Copy MCP URL**. The URL is the environment's address followed by `/mcp`, for
example:

```text
https://<environment-address>/mcp
```

The copied URL uses the route this device is connected over, so an agent on the
same device can reach it.

- **An agent on your own computers** can use `localhost` on the host or any
  `https` address, such as
  [Tailscale HTTPS](./remote-access.md#tailscale-https). Signing in sends the
  agent's credentials to the environment, and agents only do that over `https`
  or to `localhost`. A plain-`http` LAN or Tailscale address, like
  `http://100.x.y.z:3773`, lets an agent find the environment but not finish
  signing in.
- **A hosted agent**, such as ChatGPT or a bot running in the cloud, cannot reach
  the environment. Onicode does not publish its MCP server to the internet, even
  with [public access](./remote-access.md#public-access-for-guest-links-and-webhooks)
  on, and a Tailscale address only works from your own tailnet.

## Approve a sign-in

The first time an agent connects, it opens a sign-in page on the environment.
The page names the agent and where its access goes:

- For an agent on your computer, approval returns to a `localhost` address on
  the computer that opened the page.
- For an agent on another machine, approval goes to the `https` address it
  names. Anyone who runs that address gets the access.

Only approve a sign-in you just started. To approve, enter a pairing code from
**Settings → Sharing & access** or from `onicode auth pairing create` on the host. A
browser already signed in to the environment with permission to manage access
can approve without a code.

Then choose what the agent may do. **Auto** is selected by default, or the
closest lower mode allowed by your organization’s policy:

- **Read only** lets the agent read projects and threads and see
  the available providers and models. It cannot change anything.
- **Supervised** through **Full access** also let it start, message and stop
  threads in every project. A thread it starts or steers cannot run with more
  permissions than the mode you chose.

## Claude Code

```sh
claude mcp add --transport http onicode https://<environment-address>/mcp
claude mcp login onicode
```

`claude mcp login` opens the sign-in page in your browser. On a machine
without a browser, add `--no-browser`, open the printed URL elsewhere, and
paste the final URL back when asked. `claude mcp list` shows `onicode` as connected
once you approve.

## Codex

```sh
codex mcp add onicode --url https://<environment-address>/mcp
codex mcp login onicode
```

`codex mcp login --no-browser` prints the sign-in URL instead of opening it.
`codex mcp list` shows `onicode` with OAuth once you approve.

## Other agents and bots

Any agent that supports remote MCP servers over HTTP with OAuth can connect. Give
it the MCP URL and choose OAuth, and the agent finds the sign-in settings itself.
Agents without OAuth support cannot connect.

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

Approved agents appear under **Settings → Sharing & access** like other clients.
Revoke one there to cut off its access immediately. A sign-in lasts 30 days;
after that the agent asks you to approve it again.
