# Computer use

Agents can see and control the desktop apps on the machine that runs them:
read a window, click, type, pick a menu item, take a screenshot. Onicode does
this with [cua-driver](https://github.com/trycua/cua), which it downloads and
runs itself. It works on macOS, Linux and Windows.

## Turn it on

Open **Settings → Devices → Computer** and turn on **Agents can use this
computer**. The setting is off by default. Onicode downloads the driver the
first time, about 45 MB, into the Onicode home. The tools reach agent sessions
that start after that; restart a session that was already open.

A project can override the setting: choose the project in the settings scope
and change the same switch.

On macOS the driver uses the permissions of the app that runs the server.
**Permissions** shows whether Accessibility and Screen Recording are allowed,
and **Grant access** asks for them when you are at that Mac. Linux and Windows
need no permission step, but the server has to run inside your desktop
session. On Linux under Wayland, the compositor asks before each screen
capture.

## Driver approvals

**Standard** keeps cua-driver's own checks: it refuses to act on system
security prompts, and it asks before it attaches to a browser profile you are
signed in to. **Unrestricted** removes those checks, including the driver's
guard against instructions hidden in what an app shows. Choose it only on a
machine where an agent acting on a wrong instruction costs you little.

Changing the mode restarts the driver. A thread that is already open uses the
new mode once its agent session restarts.

## Choose the providers

With computer use on, every provider gets the tools. Turn a provider off under
**Computer** to keep the tools away from its sessions. Onicode adds the tools
as an MCP server named `computer`, so a provider has to accept MCP servers to
use them. Pi needs version 0.99 or later.

## What agents can reach

The driver acts as you. An agent that can use this computer can open any app
you can open and read any window you can read, in every project that has the
setting on. One driver serves all threads, and they share one keyboard focus:
two agents working in the foreground at once get in each other's way.

Onicode turns off the driver's telemetry and its update check.
