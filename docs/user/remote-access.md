# Remote access

Connect a phone or another desktop app to Onicode running on a different
machine. That machine must stay running and reachable while you work.

Your work never passes through an Onicode cloud service. Other devices reach a machine over
your local network or your [Tailscale](https://tailscale.com) tailnet. Only
[mobile notifications](./mobile-notifications.md) leave it, through a push gateway.

## Set up a command-line host

```bash
onicode up
```

It asks which devices may connect: only this machine, devices on your tailnet, or devices on
your local network and tailnet. Then it runs Onicode as a
[background service](./background-service.md). Run `onicode up` again to change your choice,
and `onicode status` to see it.

`onicode up --lan`, `--tailnet-only`, or `--no-lan` answers without asking, and `--yes` takes the default. Without a
terminal and with no choice named, `onicode up` stops and names the flags. `onicode status --json`
prints the settings, and `onicode up --help` lists every flag.

## Pair over a LAN or private network

Use direct pairing when the other device can reach the host's network address.

On a desktop host, open **Settings → Sharing & access** and set **Network access** to
**Tailnet only** or **LAN and tailnet**, then create a pairing link using an address the
other device can reach. Changing network access restarts the desktop app. Set it back to
**Off** in the same place.

For a command-line host, choose the local network in `onicode up`, or run:

```bash
onicode up --lan
```

For a server in a terminal, run `onicode serve --lan`. Either way, Onicode then
accepts connections from your local network and tailnet, so a pairing code is
all that keeps other devices on them out. Use `--tailnet-only` instead of
`--lan` to accept only devices on your tailnet.

If a server is already running, generate a fresh link without restarting it:

```bash
onicode pair
```

Scan the QR code in **Add environment** on your phone and it pairs right away.
Scanned with the phone's camera instead, the code opens Onicode with the address
filled in for you to confirm. You can also paste the pairing URL into **Add
environment**. Connection settings are under **Settings → Environments**
on desktop and **Settings → Environments** on mobile. A loopback address
such as `127.0.0.1` reaches only the device opening the link.

On a computer with the desktop app, you can open the printed `onicode://pair`
app link instead. The desktop app asks you to confirm the server address, then
adds it.

If your HTTPS endpoint uses a certificate from your own certificate authority,
install that CA on Android as a **CA certificate** in its security settings; the
Android app then trusts it.

Pairing authorizes that device for future connections. Use a fresh one-time link
for each new device; you do not need the original token to reconnect. Links
created in Settings can only be copied from the client that created them while
its Sharing & access page stays open. If you leave or reload that page, create
another link to share.

### Reach one machine several ways

A machine can have more than one route: LAN, Tailscale, another VPN, a public
URL, or SSH. Tailscale shares its `100.64.0.0/10` address range with
other VPNs such as Cloudflare WARP, so an address in that range shows as VPN
unless the machine confirms it is on Tailscale. To add a route, choose **Add
route** in the machine's route list.
A pairing link for a saved machine is refused when it comes from an address
that machine does not have yet; add that address with **Add route** instead.
Pairing again over an address the machine already has still works. A new route
is placed by speed, in that order, and you can reorder routes at any time.

While connected through a paired address, Onicode also learns the
machine's current LAN and Tailscale addresses and adds them as routes, so
pairing once over Tailscale is enough to use the LAN at home. When the
machine's LAN address changes, for example after it joins another Wi-Fi network,
the learned route follows it. The machine must allow network access for its LAN
address to be learned: **LAN and tailnet** under **Network access** in the desktop app, or the
[local network](./background-service.md#local-network-access) choice in `onicode up`.
You can reorder a learned route, but not remove it; it
goes away with the route it was learned through, or when the machine stops
reporting that address.

Onicode connects over the first route that answers. Away from home, a LAN
address that does not answer is checked briefly and skipped. It is only tried
again, after the other routes, if none of them connect. While connected over a
later route, Onicode checks the earlier ones when your network changes, when you
return to the app, and every minute, and moves back as soon as one works. A
route whose machine refused this device's sign-in is marked **Sign-in
refused**; pair that address again with **Add route** to use it.

On desktop, select the route count under the machine's name in
**Settings → Environments** to see its routes. Drag a route to change the order,
or remove it. On mobile, open the machine under **Settings → Environments** and
choose **Edit**.

Open **Permissions** next to **Routes** on desktop, or **Your permissions**
in the mobile route details, to see what your current connection can do on that
environment. For a remote environment, this is in its route details. Permissions
shown there apply only to the route marked **In use**; other routes are not
checked.

### See a machine's hardware

**Settings → Environments → Hardware** on desktop lists every connected machine with
its model, operating system, processor, memory, and storage, and how much of each is in
use. On mobile, open the machine under **Settings → Environments**.

Storage lists only the volumes Onicode keeps work on: your home folder, Onicode's data,
worktrees, and project folders. A machine on an older Onicode version shows its
hardware after it updates.

When a connected machine is in trouble, a warning icon appears among the icons at the
bottom of the desktop sidebar and beside the Onicode name on mobile: **Memory is slowing
agents down** (this can happen before memory runs out), **Almost out of disk space**
(where Onicode keeps its data and worktrees), or **Machine overloaded** (work waiting for
a CPU or for the disk). A machine that is merely busy shows nothing, since agents and
builds often use every core. One icon covers every machine in trouble. Click it on
desktop for details, or tap it on mobile to open the machine's page. On Linux the details
name the agents using the most memory or CPU, and you can force stop one there to free
the machine for the rest; their threads also show a warning icon in the thread list. On
Windows, only low disk space warns.

### Balance new threads across machines

Auto balance is off by default. On desktop, enable it in
**Settings → Environments → Load balancing** to automatically choose a machine for
new threads in projects grouped across connected environments. The section
appears once two or more machines are switched on.
Each machine starts at **Normal**. Choose **Prefer** to favor it when it has CPU and
memory available, **Less often** to reduce its share, or **Manual only** to exclude
it from automatic selection. These are preferences, not fixed traffic percentages.
Preferences are saved separately in each client.

The composer checks eligible machines when choosing a draft's environment, then keeps
that choice stable. Choose **Auto balance** again to check current resources, or choose
a specific machine to override it. Choosing a branch or worktree also keeps the draft
on that machine. Existing threads stay where they started. While all eligible machines
are full, the composer keeps checking until one has room. If resource checks are
unavailable, choose a machine manually to continue.
Mobile keeps its manual environment selection.

### Connect from your tailnet

When a connected machine runs Tailscale, choose **Add environment → Tailscale** to see the
devices on its tailnet, grouped by owner, with tagged devices last. Devices running an Onicode
server that answers on its default port, 3773, are marked. On mobile, the list appears in
**Add environment** once the phone is connected to a machine on the tailnet.

Picking a server with **Tailscale sign-in** on connects right away. For any other server,
Onicode fills in its address and asks for a pairing code.

To let devices sign in without a code, open **Settings → Sharing & access** on the host and
turn on **Tailscale sign-in**. Network access must be **Tailnet only** or **LAN and tailnet**.
On mobile, open the machine under **Settings → Environments** and use the switch under
**Sharing**. For a command-line host, use either switch from a connected app, or set
`"tailnetSignIn": true` in its `settings.json` under `~/.onicode/userdata`. Onicode then
asks Tailscale who each connecting device is and signs in:

- Devices logged in to Tailscale as the same user as the host. A tagged host has no user, so
  this does not apply to it.
- Devices your tailnet policy grants the `onicode.com/cap/environment` capability.

A grant also works with Headscale 0.29 and later. For example, to let a group reach tagged
Onicode hosts read-only:

```json
"grants": [
  {
    "src": ["group:reviewers"],
    "dst": ["tag:onicode"],
    "ip": ["tcp:3773"],
    "app": { "onicode.com/cap/environment": [{ "access": "read" }] }
  }
]
```

`{ "access": "read" }` lets those devices view threads, terminals, and files. Any other value,
such as `{}`, gives the same permissions as a default pairing code. Sign-in only works when
the connection arrives over the tailnet itself; `tailscale serve`, and Tailscale running in
userspace networking mode, hide who is connecting, so those devices still need a pairing
code. Signed-in devices appear under **Authorized clients**, where you can revoke them.

### Tailscale HTTPS

Join both devices to the same tailnet. In the desktop app, enable **Tailscale
HTTPS** in **Settings → Sharing & access**. Turn it off there to remove that route.

Your tailnet needs MagicDNS and HTTPS certificates enabled; see
[Tailscale's HTTPS setup](https://tailscale.com/docs/how-to/set-up-https-certificates).
On Linux, Onicode also needs permission to manage Tailscale; see
[operator permission](https://tailscale.com/docs/reference/troubleshooting/linux/linux-operator-permission)
for non-root users. If any of these is missing, Onicode still starts but can't
add the HTTPS route.

To start a command-line server with Tailscale HTTPS:

```bash
onicode serve --tailscale-serve
```

For an already-running server:

```bash
onicode pair --tailscale
```

The pairing link uses an address such as `https://machine.tailnet.ts.net/`.
The mapping created by `pair --tailscale` persists across restarts. Remove its
default-port mapping with:

```bash
tailscale serve --https=443 --set-path=/ off
```

If that port is already in use, choose another with
`--tailscale-serve-port`. See `onicode pair --help` for other pairing options.

### Public access

By default, Onicode is only reachable on your machine, local network, or tailnet. To let external
services or cloud apps reach specific endpoints on an environment, open **Settings → Sharing &
access** (under **Connections**) on desktop — or **Settings → Environments →** the machine on
mobile — and use the switches under **Public access**. Both switches are off by default:

- **Public webhooks** publishes `/api/hooks/…` so services on the internet, such as GitHub, can
  call the webhook addresses of [scheduled tasks](./project-settings.md#webhook-automations) and
  [workflows](./workflows.md#triggers).
- **Public agents** publishes `/mcp` and its sign-in endpoints so cloud apps, such as ChatGPT and
  Claude, can connect as [outside agents](./outside-agents.md). Every app still needs your
  approval.

Turning either switch on starts [Tailscale Funnel](https://tailscale.com/kb/1223/funnel) at an
address such as `https://<machine>.<tailnet>.ts.net:8443`. Turning one switch off stops its routes
immediately; turning both off removes the Funnel route. Nothing else of Onicode's API is ever
public.

Funnel needs:

- Tailscale installed, running, and logged in on the machine.
- HTTPS certificates turned on for the tailnet, under **DNS** in the Tailscale admin console.
- The `funnel` node attribute for this machine in the tailnet policy file, under **Access
  controls**. Without it Tailscale refuses, and Settings says so.
- Port 8443 free of other `tailscale serve` or `tailscale funnel` routes on the machine.
- On Linux, [operator permission](https://tailscale.com/docs/reference/troubleshooting/linux/linux-operator-permission)
  for the user running Onicode (for example, `sudo tailscale set --operator=$USER`).

If public access cannot start, Settings shows why. Fix the cause; Onicode tries again every 30
seconds. There is no hosted relay: requests sent while the machine is offline or the switch is off
fail, and nothing is held for later.

A [managed policy](./managed-policy.md#clients) whose `clients` leaves out `remote` keeps public
access off. A policy with `webhookTasks: false` also blocks **Public webhooks**.

## Desktop-managed SSH

In the desktop app, open **Settings → Environments → Add environment**, choose
**SSH**, and enter a host or SSH alias such as `user@example.com`. Onicode starts
or reuses a server there and opens the port forward for you. Projects, provider
credentials, and agent work stay on the remote machine.

The remote host must be Linux or an Apple Silicon Mac with `curl` or `wget`,
`tar`, `sha256sum` or `shasum`, and [provider setup](./install.md#providers).
The first launch downloads Onicode's server to `~/.onicode/runtime` on the host, so
it takes longer than later ones.
Provider CLIs must be on the `PATH` of a non-interactive login shell there;
check with:

```bash
ssh user@example.com 'sh -lc "command -v claude codex"'
```

Onicode cannot answer a second sign-in prompt such as a Duo code. On macOS and
Linux it can reuse a connection you signed in to in a terminal instead: turn on
connection sharing for the host in `~/.ssh/config`, run `ssh <alias>`, then
connect. When that connection closes, sign in again in a terminal and retry.

```sshconfig
Host <alias>
  ControlMaster auto
  ControlPath ~/.ssh/cm-%C
  ControlPersist 8h
```

If SSH reconnecting fails after an app update, retry the launch once. Removing
the connection stops a server that Onicode launched; a server that was already
running is left alone.

Each desktop app keeps one session on the host for its SSH connection and reuses
it when it reconnects. Revoke sessions you no longer use in **Settings →
Sharing & access** on the host, or on a headless host with `onicode auth session list` and
`onicode auth session revoke <id>`. A desktop that can still sign in over SSH pairs
again on its next connect.

For Antigravity's Google callback on a remote host, see
[remote sign-in](./providers-antigravity.md#sign-in-from-a-remote-device).

## Browser on a remote environment

Browser tabs belong to the environment, so you and your agents see the same
tabs from any device. The desktop app shows its own environment's tabs
directly. Every other device, and the desktop app for other environments,
streams them from the host. Agents keep using them while no device is
connected, and `localhost` addresses reach servers on the host.

A tab you open yourself in the desktop app for another environment loads on
your computer instead, so the element picker, screenshots, and annotations work
as they do locally. Its `localhost` addresses still reach servers on the host,
over the connection the app already has, whichever way you connected. If the
host cannot be reached, the tab shows a connection error; a `localhost`,
`127.0.0.1`, or `[::1]` address never opens a server on your own computer.
Other names that point at your computer, such as `/etc/hosts` entries, still
do. These tabs keep their own cookies and site data, apart from anything your
own `localhost` servers set. They ignore your system proxy settings, so they
can't reach a site you reach only through a proxy. Over SSH, a relay, or a
connection saved at a `localhost` address (such as your own `ssh -L` forward),
your system browser can't reach the host's `localhost`, so Open in browser isn't
offered for those pages. The mobile app streams tabs from the host.

The desktop app can also forward a port on the host to your computer, so any
app there, such as your own browser, `curl`, or a database client, reaches it
at `localhost`. Use the forward button on a server in the thread's **Servers**
list, or **Forwarded ports** in the command palette, where you can also type
any port number and stop forwards. A forward keeps the host's port number
unless that port is taken on your computer, in which case it uses a free one
and shows it. Forwards end when you stop them or quit the app. ⌘-click
(Ctrl-click on Linux) on a remote server forwards it and opens it in your
browser.

A forwarded port behaves like a server running on your computer: anything on
your computer can reach it, including web pages open in your browser, so a
forwarded service is exposed the same way a local dev server is. Forwards
listen only on your computer's loopback, never on your network. On Linux they
accept only apps running as your user; on macOS, other users signed in to the
same computer can reach them too.

On iPhone, iPad, and Android, a tab showing one of the host's `localhost` pages
has **Open on this device** in its menu. So does a long press on a `localhost`
link in a chat. It opens the page in the phone's own browser view, at the same
`localhost` address, so it scrolls and responds to touch natively. Like the
desktop app, it never opens a server on the phone itself. The streamed tab
stays where it was, and agents keep using it. Each host keeps its own cookies
and site data on the phone. On iPhone and iPad the page runs under a stand-in
address while the address bar shows `localhost`, so browser features that need
a secure page, such as the clipboard or service workers, aren't available there.
The page sees that address wherever the pages, scripts, and other text the
host sends over plain HTTP say `localhost` or `127.0.0.1`, which is how its
requests to the host's other ports reach the host. A request to an address it builds any other way, such as
`[::1]`, goes to the phone and fails. A page at such an address, such as
`[::1]` or `127.0.0.2`, doesn't open on iPhone and iPad; use the streamed tab
for it.

The phone shows one host's `localhost` pages at a time on Android. If another
host's page is still open behind the one you're looking at, close it, then try
again.

If the host runs an older Onicode, or your connection to it has no terminal
access, `localhost` tabs you open in the desktop app stream from the host
instead, and the mobile app doesn't offer **Open on this device**; tapping a
`localhost` link in a chat says why it can't open there.

All of this rides the host's preview tunnel. Turn off **Settings → Sharing &
access → Preview tunnel** on the host to stop other devices from loading its
`localhost` pages themselves: port forwarding and **Open on this device** go
away, and open tunnels close. Tabs keep working by streaming from the host, so
anyone who can use the host's browser tabs can still view its `localhost` pages
there. An administrator can also turn it off with a
[managed policy](./managed-policy.md#other-capabilities). While the host's
`settings.json` is malformed, the preview tunnel stays off unless a managed
policy turns it on.

Tabs run in the same headless browser [HTML renders](html-renders.md) use: an
up-to-date Chrome, Brave or Chromium installed on a macOS host, otherwise a
headless Chrome, about 120 MB, that the first tab downloads into the Onicode
home. Some Linux hosts need [setup](#browser-host-setup) before it can
start.

Agent tabs have separate storage and share a Chromium process. Take control before
typing into an agent's tab, then release control when you want the agent to
continue. Read-only connections can watch without changing the page.

While you have control, the tab works with your device: text the page copies or
cuts goes to your clipboard, a file picker on the page opens your device's
picker, a dropdown on the page shows its options on your device, and a finished
download is offered for you to save. Popups such as sign-in windows open as
their own tabs. Downloads stay on the host until the tab closes. Audio does not play on your device.

On a phone, tap the floating preview's corner dot to show its controls, then
**Pop into separate window** to keep watching in picture-in-picture over other
apps.

### Local servers

The browser lists ports held by processes your terminals and agents started.
Onicode reads which process holds each port and never connects to it, so a
listed port opens as `http://localhost:<port>` and might not be a web page. For
an HTTPS server or a specific path, set the **Preview URL** on the project
script that starts it; Onicode checks that address and opens it as written.

To stop listing ports, set `ONICODE_PORT_DISCOVERY=0` in the environment that
starts the Onicode server, such as
[`service.env`](./background-service.md#configure-the-service-environment) for
the background service, and restart it. Script preview URLs still appear.

### Browser host setup

macOS, Windows, and Linux desktops run the browser as is. Some Linux hosts need
one-time setup: Ubuntu 23.10 and later block the sandbox the browser runs in,
and minimal images and containers lack libraries it loads. When that happens,
the server says so at startup, and browser tabs and HTML previews show the
command to run on the host:

```sh
sudo onicode browser setup
```

The server shows the exact line for how you started it, and keeps your `PATH`
when you started it with `npx` and Node is installed only for your user. Where `onicode` is not on your `PATH`, such as with only the
desktop app installed, it names the full path of the app's own `onicode` instead. It allows Chrome's sandbox with an AppArmor profile and installs
any missing libraries and system fonts with apt; pages that load without text
can mean the host has no fonts. It is safe to run again, and each run reloads
the AppArmor profile: run it again if the browser reports a blocked sandbox on a
host restored from an image or snapshot, where the profile can be on disk
without being loaded. Without `sudo`, it only reports what it would change.

The browser runs in Chrome's sandbox by default. Chrome cannot start that sandbox
as root. If your Onicode server runs as root, run it as a nonroot user to keep
sandboxing enabled. `onicode browser setup` does not fix this root restriction.

If you need to run as root, or cannot enable the sandbox on your host, set
`ONICODE_SERVER_BROWSER_SANDBOX=0` in the **Onicode server's environment** and restart
the server. This disables Chrome's sandbox. For a manual launch:

```sh
ONICODE_SERVER_BROWSER_SANDBOX=0 onicode serve
```

For a systemd service, add `Environment=ONICODE_SERVER_BROWSER_SANDBOX=0` under
`[Service]` in its configuration, run `sudo systemctl daemon-reload`, then restart
your Onicode service. Setting the variable in an agent's terminal does not change the
environment of an already running Onicode server.

## Remote desktop

See and control a machine's whole screen from the desktop app or your phone,
for the moments an agent's work needs a GUI: a permission prompt, a simulator
that will not boot, an installer.

On the machine you want to reach, open the Onicode desktop app and turn on
**Settings → Sharing & access → Remote desktop → Share this screen**. macOS
asks for Screen Recording, and for Accessibility so viewers can use the mouse
and keyboard. The setting is per machine and can only be turned on from that
machine. Keep the app running there; while sharing is on, closing its window on
macOS hides it instead.

Then, from another device:

- **Desktop:** open the command palette and choose **Open remote desktop**, or
  use the screen button on the machine's row in **Settings → Environments**.
  The header has a full screen button; in full screen, hold Esc to leave.
- **Phone:** **Settings → Environments →** the machine **→ Remote desktop →
  View screen**.

On a phone the screen works like a laptop trackpad. Your finger moves the
pointer from wherever it is, and taps click at the pointer, so a small target
is easy to hit:

| Gesture                        | Result                             |
| ------------------------------ | ---------------------------------- |
| Drag with one finger           | Move the pointer                   |
| Tap                            | Click                              |
| Tap twice                      | Double-click                       |
| Tap with two fingers           | Right-click                        |
| Tap with three fingers         | Middle-click                       |
| Tap, then touch again and drag | Drag, or select                    |
| Touch and hold, then drag      | Drag, or select                    |
| Drag with two fingers          | Scroll                             |
| Pinch                          | Zoom; the view follows the pointer |

The screen opens zoomed in far enough to read, on the machine's pointer, and
coming back to a screen returns to where you were reading. The magnifier in
the toolbar switches between that and the whole screen. Opening the keyboard
does not resize the picture: it only slides up when the keyboard would cover
the pointer, and slides back when the keyboard closes.

Turn the phone sideways for a wider picture. The toolbar moves to the side,
and comes back above the keyboard while you type.

The **⋯** menu switches to **Direct touch**, where a tap clicks under your
finger, one finger moves the picture, a long press right-clicks, and a long
press followed by a drag drags, with a magnifier above your finger showing
what is under it. Two fingers scroll and pinch as before. It
also has **Paste from this device**, which pastes your phone's clipboard into
the machine, and the shortcuts a phone cannot type, such as ⌘Tab. The toolbar
opens the keyboard and has the keys a phone keyboard lacks, through F12;
⌃ ⌥ ⇧ ⌘ apply to the next key, click or drag. Press one twice to keep it held
until you press it again.

Things to know:

- The picture travels directly between the two devices, not through the
  Onicode server, so they need a route to each other: the same network or the
  same tailnet. A connection that only reaches the server through SSH
  forwarding or a public HTTPS proxy cannot open the screen.
- Anyone whose connection may use that machine's terminals can view and
  control its screen while sharing is on. **Sharing & access** lists who is
  watching and can disconnect them.
- Control works on macOS hosts. Windows and Linux hosts share their screen
  view-only for now.
- A host with several displays shows one at a time; pick it in the header on
  desktop or the toolbar on a phone.
- Shortcuts your own device reserves, such as ⌘Tab, never reach the app. On
  desktop, send them from the keyboard menu in the header.
- Text you copy on one side can be pasted on the other. A copy on the shared
  machine reaches every connected viewer; the desktop app sends its own
  clipboard when you click into the picture, and a phone sends it when you
  choose **Paste from this device**. Only text is shared, up to 64,000
  characters. If macOS on the shared machine asks before apps read the
  clipboard, **Settings → Sharing & access → Remote desktop** there says so
  and names the setting to change.
- A dropped connection, such as a phone moving from Wi-Fi to mobile data,
  reconnects by itself for up to 30 seconds. If the shared machine ends the
  session, the viewer stops and offers **Retry**.
- The shared machine's display stays awake while someone is connected.
- Sound is not sent.

## Link another environment

Link two machines, such as your laptop and a VPS, and agents on one can work on
the other: launch, message, wait on and stop its threads, answer their
questions, and manage its queues, projects and scheduled tasks.

Linked environments is a beta. Turn it on for both machines in **Settings →
Labs**; on mobile, **Settings → Labs** lists each connected machine. On a
headless server, pair the desktop app or your phone with it and use that
client's **Settings → Labs**, or add `"linkedEnvironmentsEnabled": true` to
`settings.json` in its `userdata` directory, such as
`~/.onicode/userdata/settings.json`; a running server picks it up. While it is
off on either machine, agents start nothing over a link between them. A task
already running on the other machine still finishes, and if that machine has it
off, its result arrives once it is back on.

On the machine you will work from, open **Settings → Environments → Linked environments**,
choose **Link**, and pick the other machine or enter its address. The list
holds the machines this app knows. If this
app has an address for that machine and is connected to it with a session that
manages access there, it makes the pairing code itself; otherwise paste one
from that machine's **Settings → Sharing & access**. An address you type always
needs a pasted code. From a terminal, use
`onicode environment link <address> --pairing-code <code> --access <mode>`.

To link many machines at once, choose **Link several…** (it appears once this
app has two other machines saved). Tick the machines, pick one access level for
all of them, and confirm once. A machine can be ticked when this app is
connected to it with a session that manages access there and has an address
another machine can reach; the list says why when it cannot. Each machine
reports its own result, and the ones that failed stay ticked so you can try
only those again. Machines already linked are left as they are.

For machines no app is connected to, put one `<address> <pairing-code>` per
line in a file (make each code with `onicode pair` on that machine) and run
`onicode environment link --from <file> --access <mode>`, or `--from -` to read
standard input. It prints a result per line, leaves machines already linked at
that address as they are, and exits non-zero if any line failed, so the same
file can be run again with fresh codes for the failures. A machine linked at
another address is refused: forget that link first if it moved.

You can also ask an agent to link a machine by name, such as "delegate this to
my vps". It shows a card in the thread where you pick the machine the same way
and the access, then approve. An address the agent gives that this app does
not know for one of your machines also needs a pasted code. Paste one only for
an address you trust: the code is sent there.

- The access you pick is the most agents from this machine may do there. An
  agent also never gets more there than its own mode here.
- Threads a link starts are marked **From** and this machine's name there.
- A full-access link works there like that machine's own agents. Below full
  access, the link's agents can change only threads it started, not that
  machine's own threads, projects or settings, and they skip its project setup
  scripts.
- The link shows up in that machine's **Settings → Sharing & access** with its
  other clients, and can be revoked there. **Forget** on this machine also ends
  it there when that machine confirms it. If it can't, such as when it is
  offline or on an older Onicode, Forget says so; revoke it in that machine's
  Sharing & access.
- A link stays valid while agents use it. One unused for 30 days expires, as
  does one to a machine on an older Onicode after 30 days; link again with a
  new pairing code.

On desktop, **Settings → Environments** opens as a map of your machines:
this app, each environment with one line per route (the one in use is
highlighted), and the machines an environment links to. Select a machine to
manage it, or switch to **List** for the same controls as rows. To link from
the map, drag the plus on an environment onto another machine, or press it and
pick one. If this app is connected to both machines, knows an address the first
can reach the other at, and its session on the other one manages access there,
it needs no pairing code.

A machine this app is not connected to, shown only because another environment
links to it, can still be linked without a code when that link has full access:
drop the plus on it and the environment holding the link gets the pairing code
from it. Your session on that environment must manage access there, already
have the access you pick, and it must
run a version of Onicode that can get the code. The result
is a separate link with the access you pick, listed and revocable on the linked
machine like any other. Links still do not chain.

A link goes one way. To let the other machine's agents work back, tick **Also
link … back** when you create the link, or select a one-way link and use
**Link back…**. It is offered when this app is connected to both machines and
may manage access on each. Two machines linked to each other at the same access
are drawn as one line with an arrowhead at each end; they are still two links,
and each is forgotten on its own.

To link one environment to several machines at once, Shift-click the machines
(or Shift-drag a box around them, or press Space on a focused machine), then
drag the environment's plus onto any of them, or use **Link several…** above
the map. Dropping the plus on a stack offers the stack's machines. The dialog
ticks the machines it can link without a pairing code and says why for the
rest. When this app is connected to none of them, the map offers to link them one
at a time instead and says how many need a pairing code.

Large setups fold: machines known only through links that share the same
linking environments and access levels become one stack, and once this app
knows more than 12 environments, the ones that are not connected and have no
links fold by state. Select a stack to list and filter every machine in it.
Opening a stack on the map draws its first 24 below its card: the rest of the
map stays where it is and the map grows taller, so pan or press the fit button
to see all of it. **Find a machine** above the map searches by name, and Enter
goes to the first match. The **?** button above the map lists these gestures.

The map draws the links of the machines this app is connected to; a machine it
is not connected to may hold links the map cannot show. A link reaches one
machine and does not carry on through that machine's own links.

An agent can also hand a task to a linked machine with `delegate_task`: it runs
there as an ordinary thread, in the project with the same repository (or the
one `target.projectId` names), and this thread wakes with its result when it
ends, as for any delegated task. It keeps following the task across restarts.
If the other machine revokes the link, or the link is removed here, the task
fails with that reason.

### Read a linked machine's threads

You can read a thread on a linked machine without connecting this app to it. Open
a task that was delegated there from the thread's agents, or a thread link that
points there, and the thread opens read-only, streamed through the machine you
are connected to over its link. It follows the thread live. On desktop
it opens as a tab in the right panel, beside the thread you are in, or in the
main view with ⌘/Ctrl-click, like
[subagents and side chats](thread-sidebar.md#open-a-thread-beside-the-one-you-are-in).
On mobile it opens in the inspector on layouts that have one, where Maximize
fills the screen, and as its own screen elsewhere. It shows the same timeline as
any other thread on desktop, and a shorter read-only list on mobile.

- The view is marked **Read-only** with both machines' names. To reply, approve
  or stop the thread, choose **Connect to interact**. When the link has full
  access, this app pairs with that machine in one step and opens the thread
  there; you need permission to manage access on the machine you are connected
  to. Otherwise, if that machine has **Tailscale sign-in** on and this device
  reaches it over Tailscale, it signs this device in. Failing that, the
  add-environment form opens for that machine and asks for a pairing code you
  create on it.
- This app connects to that machine directly, so it must be able to reach one of
  the addresses the link uses. If it cannot, the view says so and stays
  read-only. The new connection appears in that machine's **Settings → Sharing &
  access**, named after this device and the machine it came through, and is
  revoked there like any other.
- Files, images and published visuals stay on that machine and do not load
  here. A command's output shows as of when you open its row.
- If this app is already connected to that machine, the thread opens there as
  usual instead.
- You need permission to read threads on the machine you are connected to, and
  Linked environments must be on there. Both machines need a version of Onicode
  that has this view.

## Manage or revoke access

On the host, **Settings → Sharing & access** lets authorized administrators create
pairing links, connect [outside agents](./outside-agents.md), and revoke client and agent
sessions. Revoking an unused link prevents new pairings; revoke a session under **Authorized
clients** to remove its access immediately. Command-line management is available through
`onicode auth --help`.

When the app connects to more than one environment, choose one at the top of the
page. A desktop app with **Local environment** off can manage a background
service on the same machine this way.

A session with an open connection stays listed after its access credential
expires.

To choose a token's permissions, pass `--scope` once for each scope you want:

```sh
onicode pair --scope orchestration:read --scope terminal:read
```

The selected scopes replace the default permissions. The same option works with
`onicode auth pairing create` and `onicode auth session issue`; each command's
`--help` lists the available scopes. Without `--scope`, pairing tokens and issued
bearer sessions get every permission, including managing access.
A terminal or a full-access agent on the host can reach those as well, so a
client that should not manage access also needs to go without `terminal:operate`
and `orchestration:operate`.

To change an existing client's permissions, create a fresh pairing link with the
scopes it needs. For mobile or a saved remote environment on desktop, use **Add Environment** with the fresh link or code; pairing
the same environment replaces its saved grant. Reconnecting alone does not change
permissions.

Grouping checkouts does not combine their permissions. Shared project settings
require `orchestration:operate` on every member environment; actions on one
checkout use that checkout's permissions.

`source-control:write` covers direct Git and pull request changes made from the
client: pushing, switching or creating branches, cloning, and removing
worktrees. It does not restrict what a task does. Starting a task in a new
worktree still creates that branch and worktree with `orchestration:operate`,
and the agent it runs can use Git however the environment allows. A project
that [lays out its worktrees with its own command](multi-repo-projects.md#lay-the-folder-out-yourself)
runs that command on the host, so creating a worktree there from the client also
needs `orchestration:operate` or `terminal:operate`; without one of them,
removing a worktree skips the project's `remove` command. The command also has
to be [approved](multi-repo-projects.md#approve-the-commands) first, from a
client with `terminal:operate`.

Settings changes, provider management, environment maintenance,
and access management can each be granted separately. New
pairings without `--scope` include all of them. Existing clients keep the
permissions they paired with and can stay connected after an update, but newly
separated features may require pairing again with the permissions they need.
Older clients may show controls that the server denies. Create a fresh pairing
link to change a client's permissions.

`filesystem:read` allows browsing host files, opening workspace files, and viewing
local changes. Add `filesystem:write` to allow editing files or saving plans to
the workspace. These scopes control direct file access from the client.

Treat pairing URLs and authorization codes as passwords. Do not include them in
screenshots, logs, or bug reports.

## Using the Desktop App as a Remote Only

If a computer should only drive work running elsewhere, turn off its local environment. In the
desktop app, open **Settings → Environments** and switch off **Local
environment**. Onicode restarts without a local server: no local agents or terminals run, WSL
backends stay off, and other devices can no longer connect to this computer. Your projects,
history, and saved connections are kept, and you keep working through pairing or SSH.
If a [background service](./background-service.md) on this computer uses the same Onicode home, the
app still treats it as this computer: **Open in** and file reveal for its projects run here,
whichever connection you reach it through, and `onicode app` and folders dropped on the sidebar
open on it.

Switch **Local environment** back on in the same place to restart with your previous local
settings.
