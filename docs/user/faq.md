# FAQ

## Is Onicode a harness?

No. A harness is what turns an LLM into an agent: it gives the model tools, a way to call them, runs them, and hands the results back. Claude Code is kind of a harness. It is an interface (the TUI, the Code tab in the desktop app) plus a harness underneath (the Anthropic Agent SDK). Codex is the same split: the app plus the Codex App Server. Onicode uses those harnesses. It is not a harness, and it does not work without one.

Chef = LLM. Kitchen = harness. Onicode = the restaurant that lets you bring whatever kitchen you want. Our MCP is a cookbook we hand the kitchen, but the chef won't open it unless you tell them to :) I wrote the long version here: [What is a harness?](https://onicode.gg/blog/post/what-is-a-harness)

Bring your own: Claude Code, Codex, Cursor, Grok Build, OpenCode, Google Antigravity, Pi, or Muse Code. [Provider setup](./install.md#providers)

## Why use Onicode instead of Claude Code or the Codex app?

We were inspired by the Codex desktop app, Conductor, Claude Desktop, and Cursor Glass, but none met our bar. What you get:

- Bring your own sub. Claude, Codex, and whatever else you already have installed.
- Best in class remote. Run `onicode up` on any box and drive it from anywhere.
- Best in class mobile app.
- Truly 100% open source. If we go the wrong direction, fork it.

Every harness feels nearly identical in here, so you can stop worrying about harnesses and focus on building :) [Install](./install.md)

## Can I use Onicode with my Claude Code subscription?

Yes.

## How much does Onicode cost?

Nothing. There's no way to pay us to use Onicode. You bring your own sub. It is free and open source (MIT). You still pay your provider like you already do. [Install](./install.md)

## Why is my Claude 5-hour limit draining so fast?

Onicode drives Claude through Anthropic's Agent SDK. I confirmed it's a real bug on Anthropic's side: all Agent SDK usage will drain your 5h limits faster than using Claude Code directly. To be clear: this does not affect weekly limits. We have reported it to Anthropic and are hoping for a fix soon. [My post on it](https://x.com/theo/status/2114166439813017873)

## How do I use Onicode remotely?

Your agents run wherever Onicode runs: your desktop, a server, a VPS. You connect to it from anywhere. I do this constantly. Less than 5% of my prompts run on my laptop. Run `onicode up` on that machine and you get really good remote support. Pair over your LAN or Tailscale; a browser on your network can open the machine's own web app. [Remote access](./remote-access.md)

## How do I use Onicode on my phone?

Get the [iOS](https://apps.apple.com/us/app/onicode-remote-claude-more/id6787819824) or [Android](https://play.google.com/store/apps/details?id=dev.onicode.app) app, then connect it to your computer with a pairing link over your network or Tailscale. The phone is a remote. Your computer does the work and needs to stay on. [Mobile setup](./install.md#mobile-app) · [Remote access](./remote-access.md)

## Will I get banned for using my subscription?

No. We work with all of the labs and Onicode is within the TOS of Claude Code, Codex, and Google Antigravity. We worked directly with [SpaceXAI](https://x.com/SpaceXSI/status/2070195830192320600) to collaborate on the Grok Build and Cursor integrations. Everything else uses official ACP bindings.

If it works in Onicode, you can trust it is safe.

## Can I add a bunch of accounts for one provider?

I don't think Onicode is the right place to manage a pile of accounts. If we add an auto-switcher, we are risking TOS bans for our users.

A separate work account is fine. Setup for two: [Codex](./providers-codex.md#use-multiple-accounts) · [Claude](./providers-claude.md#separate-accounts-or-configurations). For a bigger pool, manage them outside Onicode with a self-hosted proxy like [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI). I walked through how I use 6 Claude subs and 3 Codex subs in [this video](https://www.youtube.com/watch?v=D8PikZ1KhUo), and wrote up the setup in [this guide](https://gist.github.com/onidotgg/59a521d2ee8d70aaf90e280c175072af). Hand it to your agent and it will walk you through it. Early apologies if it gets you banned 💀 After you set up a hub, Onicode can show its pooled limits under Usage. [Connect a hub](./usage.md#connect-a-cliproxyapi-hub)

## Can I use an API key or OpenRouter instead of a subscription?

Yes. Add another provider instance in Settings and give it its own keys and endpoint. Claude has a full OpenRouter recipe. Your subscription setup stays untouched. [OpenRouter](./providers-claude.md#openrouter) · [Provider instances](./install.md#providers)

## Does Onicode support local models?

No. Onicode is a serious developer tool. Locally runnable models are not capable of meaningful engineering work.

## Does my code go to Onicode servers?

No. There are no Onicode servers. Your code and prompts go from your machine to your provider (Anthropic, OpenAI, etc.), and your devices reach your machine over your own network or Tailscale. Onicode sends no usage data anywhere.

## Worktrees are eating my disk. How do I clean them up?

Open **Settings → Worktrees → Cleanup** and turn on automatic worktree cleanup. You can also run it now from there. Worktrees add up fast. One user cleared 500 GB. [Storage cleanup](./project-settings.md#storage-cleanup)

## Is there voice input?

Yes. On iPhone and Android, dictation uses on-device speech by default; you can opt in to a paired environment instead. Web and desktop require you to choose an environment first. The transcript goes into your draft for review. See [Voice input](./voice-input.md).

## Does it work on Windows, Mac, and Linux?

Yes, all three. Desktop app, CLI, the works. One catch: there is no `onicode` binary for Intel Macs, so use the desktop app there. Windows folks can also run agents inside WSL. [Install](./install.md)

## Why is my provider not showing up?

Almost always, the CLI is not installed or not on the `PATH` of the machine that runs the server. (Not your phone. The computer.) Log in on that machine first, like `codex login` or `claude auth login`. Still missing? Point **Binary path** at it in provider settings. Version managers love to cause this one. [Provider setup](./install.md#providers)
