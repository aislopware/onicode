# Managed policy

A managed policy lets an administrator configure Onicode for every user on a machine. It sets
defaults and locks for settings, and limits such as the broadest permission mode or which
providers may run. The Onicode server on the machine enforces it for every app that connects:
the desktop app, a browser, or the mobile app.

## Roll out a policy

1. Write the policy as JSON, starting from the [example](#write-a-policy).
2. Run `onicode policy check policy.json`. It prints what the policy will do, or what is wrong with it.
3. Deploy the policy to each machine with your device management tool, in
   [one of the supported places](#where-the-policy-goes).
4. Restart Onicode on the machine. The server reads the policy only when it starts.
5. Run `onicode policy show` on the machine to
   [check what the server loaded](#check-what-the-server-loaded).

The desktop app includes the `onicode` command; see [The `onicode` command](./install.md#the-oni-command).

## Write a policy

```json
{
  "schemaVersion": 1,
  "organization": "Example Corp",
  "supportUrl": "https://it.example.com/onicode",
  "settings": {
    "defaultRuntimeMode": { "value": "approval-required", "locked": false },
    "enableAgentBrowserAccess": { "value": false, "locked": true }
  },
  "limits": {
    "maxRuntimeMode": "auto-accept-edits",
    "clients": ["desktop", "local-browser"]
  }
}
```

`schemaVersion` and `organization` are required. Users see the organization's name in Settings
and beside each control the policy locks, with a link to `supportUrl` if you set one. [`settings`](#settings)
sets defaults and locks, and [`limits`](#limits) restricts what Onicode may do.

Every user on the machine can read the policy, so do not put credentials in it.

## Where the policy goes

Use one of these per machine:

| OS      | Native policy                                                                                         | Policy file                                        |
| ------- | ----------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| macOS   | Configuration profile for `dev.onicode.app`                                                           | `/Library/Application Support/Onicode/policy.json` |
| Windows | Group Policy template, or a `Policy` JSON value (both under `HKLM\SOFTWARE\Policies\Onicode\Onicode`) | `C:\Program Files\Onicode\policy.json`             |
| Linux   |                                                                                                       | `/etc/onicode/policy.json`                         |

Onicode does not merge sources. If a machine has both a native policy and a policy file, Onicode
refuses to start until you remove one.

**Policy file.** Copy the JSON to the path for the OS. Make it and its folder writable only by
administrators.

**macOS configuration profile.** Add a custom settings payload for the preference domain
`dev.onicode.app`. The payload's keys are the policy's top-level keys (`schemaVersion`,
`organization`, `supportUrl`, `settings`, `limits`); JSON objects become dictionaries and lists
become arrays. To make the payload from a policy file you checked, run
`plutil -convert xml1 -o dev.onicode.app.plist policy.json` and upload the result, or run
`onicode policy templates <directory> --policy policy.json` to write a complete
`macos/onicode.mobileconfig`. Install it as
a device profile, since Onicode ignores user profiles, and deploy only one profile for this domain.
A profile cannot hold `null`, so use a policy file if a setting's value must be `null`.

**Windows Group Policy.** Run `onicode policy templates <directory>` and copy
`windows/onicode.admx` and `windows/en-US/onicode.adml` into your `PolicyDefinitions` folder or
central store, or import them into Intune. The policies appear under Computer Configuration >
Administrative Templates > Onicode. Set **Organization** whenever you set any other Onicode policy.
Locks and provider fields go in the two JSON policies, as the same objects a policy file would hold.
Machines pick up a change at the next Group Policy refresh; run `gpupdate /target:computer /force`
to apply it now, then restart Onicode.
Setting **Allow webhook tasks** or **Allow importing browser cookies** to **Disabled** turns that
feature off. Setting any other Onicode policy to
**Disabled** restricts nothing, the same as **Not Configured**.

**Windows registry value.** Instead of the template, set the `Policy` value (`REG_SZ`) to the whole policy as one line of JSON,
from Group Policy Preferences or an Intune script. Onicode reads it through the Windows registry
API, so it works where registry tools such as `reg.exe` are blocked for users. Use one or the other:
Onicode refuses to start when the key holds both a `Policy` value and the template's settings. In
PowerShell, as an administrator:

```powershell
$json = (Get-Content -Raw -Encoding UTF8 policy.json) -replace '\r?\n\s*', ''
New-Item -Path HKLM:\SOFTWARE\Policies\Onicode\Onicode -Force | Out-Null
Set-ItemProperty -Path HKLM:\SOFTWARE\Policies\Onicode\Onicode -Name Policy -Value $json -Type String
```

## Check what the server loaded

Run `onicode policy show` as the user who runs Onicode. It prints the installed policy and its
fingerprint, then says whether the running server loaded that policy or an older one. The server
reads the policy only when it starts, so "Restart Onicode to apply the installed policy" means it
still enforces the old one. In Settings, users see that your organization manages the environment,
and each locked control shows **Managed by**.

## Change or remove a policy

After you deploy, change, or remove a policy, restart the Onicode server on the machine: quit and
reopen the desktop app, or run `onicode service restart` for the
[background service](./background-service.md). Reconnecting a phone or browser does not reload the
policy.

To remove a policy, delete the file, remove the profile, set the Group Policy settings to
**Not Configured**, or delete the registry value, then restart
the server. Users get back the settings they had chosen, since Onicode never writes policy values
into them.

## When Onicode refuses to start

Onicode refuses to start rather than run with a policy it cannot fully enforce. That includes a
policy it cannot read or parse, a setting it does not know, a value of the wrong type, a key this
version does not support, or a machine with both a native policy and a file. The error names where
the policy came from and what is wrong.

`onicode policy check` catches the same problems in a JSON file. For a profile or registry value, check
the JSON before you convert it. For the Group Policy template, check a complete policy file that
holds the same settings, then paste its `settings` and `limits.providers.instances` objects into the
two JSON policies. A policy written for a newer version can use keys an older one
cannot enforce, so upgrade Onicode before you deploy a policy that uses them.

## Settings

Each key under `settings` is a setting's path in the server's `settings.json`, such as
`storageCleanup.logsAfterDays`. To find a path, change the setting in Onicode and look in
`settings.json` under `~/.onicode/userdata`.

- `"locked": false` sets a default. It applies wherever the user has not chosen a value of their
  own, and users can change it.
- `"locked": true` sets the value. Users and agents cannot change it, in Settings or in a project's
  settings, and Settings shows the control as managed by your organization. Removing the policy
  brings back the user's own value.

A few settings need care:

- A default cannot add an entry, such as a provider instance, because users could never remove it.
  Lock the entry instead, or set provider fields with [`providers.instances`](#providers).
- A setting that can be empty, such as `defaultModelSelection`, is managed as a whole. You cannot
  manage one field inside it.
- `defaultRuntimeMode` cannot be broader than [`maxRuntimeMode`](#permission-modes).
- `worktreeCleanup`, which projects can also set, overrides the `worktree` rules in
  `storageCleanup`. Lock it along with those rules; Onicode refuses a policy that locks only the
  rules.
- `observability` cannot be managed. Its OTLP endpoints come from each user's environment
  variables, settings file and desktop startup input, which a policy cannot override.
- Manage `idleCompaction`, not the older `compactIdleThreads`. A Claude instance's own idle
  compaction choice still wins over `idleCompaction`.
- A provider instance's fields override each other, so lock `providerInstances.<id>` as a whole,
  or pin single fields with [`providers.instances`](#providers). Onicode refuses a lock on one
  field inside an instance. A whole-instance lock covers only that instance; to cover every
  instance of a provider, use `providers.instances`.

## Limits

`limits` restricts what Onicode may do. Leave a key out to keep Onicode's normal behavior.

```json
{
  "schemaVersion": 1,
  "organization": "Example Corp",
  "limits": {
    "maxRuntimeMode": "auto-accept-edits",
    "clients": ["desktop"],
    "webhookTasks": false,
    "browserCookieImport": false,
    "providers": {
      "allowedDrivers": ["claudeAgent", "codex"],
      "instances": {
        "claudeAgent": {
          "locked": true,
          "environment": [{ "name": "CLAUDE_CODE_USE_BEDROCK", "value": "1" }]
        }
      }
    },
    "updates": { "mode": "auto", "feedUrl": "https://updates.example.com/onicode/" }
  }
}
```

### Permission modes

`maxRuntimeMode` is the broadest [permission mode](./permission-modes.md) any thread may use:
`approval-required` (Supervised), `auto-accept-edits`, `auto`, or `full-access`. It covers threads
that users start, agents start, and scheduled tasks start. A thread already above the limit cannot
run another turn until someone lowers its mode; its queued messages send once that happens. New
threads start at the limit when the user's default is above it.

With `maxRuntimeMode` set, you own each provider's launch settings: binary path, launch arguments,
config home, and the environment variables set on each provider. They come from
[`providers.instances`](#providers) or Onicode's defaults, and users cannot change them, because a
wrapper script, a launch flag, or a variable such as `NODE_OPTIONS` could otherwise turn off the
provider's approvals. Pin the variables your users need, such as a gateway URL, in
`providers.instances`.

Variables in the environment that starts Onicode itself still reach providers, and a default
binary path such as `claude` is looked up on that environment's `PATH`. The desktop app builds that
environment from settings the user controls, such as their shell profile, so a user could put their
own `claude` earlier on their `PATH`. Pin an absolute `binaryPath` in `providers.instances` for each
provider you allow.

ACP agents' own mode menus have no effect under `maxRuntimeMode`, except Antigravity's, whose modes
Onicode can place under the limit. Users still choose which ACP Registry agent runs; leave
`acpRegistry` out of `allowedDrivers` to stop that.

The limit decides which modes Onicode asks the provider for. It is not a sandbox; see
[What a policy does not cover](#what-a-policy-does-not-cover).

### Clients

`clients` lists how the server may run:

- `desktop`: as the desktop app's own server.
- `local-browser`: as a server only browsers on this machine reach, such as `npx onicode`.
- `remote`: reachable from phones and other computers over the network.

`remote` is the line that matters. A server reachable from the network also accepts browsers on
this machine, and the two local kinds differ only in how Onicode was started. On machines with the
desktop app, keep `desktop` in the list, or the app cannot start.

Without `remote`, the server listens only on a loopback address, creates no pairing links, signs
out devices that paired earlier, and keeps [public access](./remote-access.md) off. It does not start with a host other than a loopback
address or `localhost`. A [background service](./background-service.md) installed before
`ONICODE_LAN` existed keeps private-network access, so without `remote` it refuses to start.
Run `onicode up --no-lan` when its `service.env` sets no `ONICODE_HOST`, or set
`ONICODE_HOST=127.0.0.1` in `service.env`, then run `onicode service restart`. The desktop app starts
with network access and Tailscale Serve off, whatever the user chose.

When it starts, the server also turns off each Tailscale Serve route, HTTPS or TCP, that forwards
to it, such as one `onicode pair --tailscale` set up, and leaves other routes alone. It does not start
while such a route stays on, or while Tailscale's answer cannot be read; the error names the
command that turns the route off. A route whose target is a name, such as `localhost` or another
machine on the same port, also stops it, since a name can resolve to this server; change the route
or Onicode's port. If Tailscale is installed but does not answer, as on a Mac where it was never set up, the
server does not start until Tailscale is set up or removed. It does start when Tailscale is not
installed, not running, or
logged out, since nothing forwards then; a route Tailscale kept returns when Tailscale runs again,
so restart Onicode after that.

### Other capabilities

- `webhookTasks: false`: scheduled tasks cannot use webhook triggers. Existing webhook tasks stay
  but do not run, and cannot be turned back on. Users can still edit or delete them.
- `browserCookieImport: false`: users cannot import cookies from another browser into Onicode's
  browser.

### Providers

`providers.allowedDrivers` lists the providers that may run, by ID: `codex` (Codex),
`claudeAgent` (Claude), `cursor` (Cursor), `grok` (Grok), `opencode` (OpenCode), `antigravity`
(Antigravity), `fx` (fx), `kiro` (Kiro), `pi` (Pi), `muse` (Muse Code), and `acpRegistry` (ACP
Registry).
`onicode policy check` rejects an ID this version does not know. Threads on other providers stay,
marked unavailable, and users can still turn those providers off.

`providers.instances` sets fields on every instance of a provider, such as an environment variable
that routes Claude through Amazon Bedrock or a base URL for a company gateway. `binaryPath` and
`launchArgs` work only for providers that have those settings. With `"locked": true`, users cannot
change the fields you set. With `"locked": false`, the fields fill in only what the user left
empty. A `codex` entry that is locked, or any `codex` entry under `maxRuntimeMode`, also rules out
[Connect with ChatGPT](./providers-codex.md#connect-with-chatgpt), since that runtime would skip
your fields. In the same way, an `opencode` entry like that rules out a server URL, so OpenCode
starts the binary you set.

### Updates

- `mode`: `auto` keeps the normal update flow. `off` stops Onicode from checking for updates, for
  machines that get Onicode through device management.
- `feedUrl`: Onicode updates from your own mirror instead of GitHub: the desktop app, `onicode update`,
  and updates started from another device. Publish only the versions you have approved, laid out
  as on a GitHub release. The release's update files (`latest.yml`, `latest-mac.yml`,
  `latest-linux.yml`, `latest-linux-arm64.yml`, and their `nightly` versions if you allow nightly)
  and the desktop installers they list go at the URL's root. The release's `oni-*` archives and
  `SHA256SUMS` go under `v<version>/`. A `onicode` run through `npx` comes from npm, so pin it with your
  package tooling.
- `allowNightly: false`: the nightly channel is unavailable.

## What a policy does not cover

The policy controls what Onicode does. Agents run through each provider's own CLI, which reads its
own configuration, such as `~/.claude`, and which a user can also run outside Onicode. To restrict
those CLIs, use their own managed configuration, such as Claude Code's managed settings.

On a managed Windows machine, the desktop app does not run a WSL environment. A Onicode server a
user starts inside WSL reads `/etc/onicode/policy.json` in that distro, not the Windows policy.
Deploy the policy file there too if your users run Onicode in WSL.

Without `remote`, Onicode does not expose the server, but a user can still forward its local port
with other tools.

A user with administrator rights on the machine can edit or remove the policy. Onicode enforces
the policy in its own processes, which run as the user, so it stops users from changing managed
settings in Onicode but does not stop a user who modifies or replaces Onicode itself. A version
from before managed policy ignores it too. To control which builds can run, use your device
management tool's application controls.
