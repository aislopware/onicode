# Browser use

Agents can browse with [obu](https://github.com/aislopware/oni-browser-use), a
browser driver built for agents. It sends an agent less of each page than a
general automation tool does, keeps working when a page changes under it, and
gets through sites that block automated browsers. Onicode downloads obu and
runs it itself. It works where the server runs on a Mac with Apple silicon or
on Linux x64.

## Turn it off or on

Agents browse with obu by default. To change that, open **Settings → Browser →
Agent access** and switch **Agents browse with obu**. Onicode downloads obu into
the Onicode home when the server first starts with the setting on. A change
reaches agent sessions that start after it; restart a session that was already
open. Where obu has no build for the server's machine, agents keep the Browser
panel's own page tools.

A project can override the switch: choose the project in the settings scope
and change it there. Which browser obu drives is one choice for the whole
environment.

## Choose the browser

**Browser panel** is the default. The agent drives its own tab in the Browser
panel, so you watch it work, and you can take the tab over at any moment; the
agent's actions are refused while you hold it. This choice needs **Agent
browser access** on, and nothing else installed. The panel opens http and https
pages only. A thread's panel tabs close when you archive or delete the thread.

**Its own Chrome** gives the agent a separate Chrome with logins of its own.
You do not see that browser. Sign-ins made there stay for later threads, and
they are shared by every thread of the environment. This choice needs Chrome
or another Chromium browser on the machine that runs the server, and on Linux
without a display it also needs Xvfb.

**Your browser** lets the agent work in the browser you use, signed in as you,
in a window of its own. When that browser allows remote debugging, obu attaches
to it: turn the option on at `chrome://inspect/#remote-debugging`, or start the
browser with `--remote-debugging-port` and enter that port in **Browser debug
port** on the desktop app. Otherwise obu starts your browser on a copy of your
profile. Tabs the agent opened close when you archive or delete its thread.

## What agents can reach

In the Browser panel an agent reaches the tabs it opened in its own thread and
the pages they load, with the cookies of the profile the panel uses. With
**Your browser** it acts as you on every site you are signed in to. Choose that
only for work where an agent following a wrong instruction on a page costs you
little.

## Limits

- Subagents that one agent session starts share that session's tab.
- obu dismisses a page's alert and confirm dialogs itself.
- The first page an agent opens in the Browser panel can take longer while
  Onicode installs the panel's browser.
- Onicode adds obu as an MCP server named `browser`, so a provider has to
  accept MCP servers to use it.
