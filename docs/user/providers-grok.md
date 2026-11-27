# Grok Build

Run xAI's [Grok Build CLI](https://x.ai/cli) in Onicode with your xAI subscription or an `XAI_API_KEY`.

## Set up Grok Build

1. Install the Grok Build CLI (`@xai-official/grok`) on the machine that runs your Onicode server.
2. Sign in on that machine:
   - Run `grok login`, **or**
   - Set `XAI_API_KEY` in the Grok instance's **Environment** settings.
3. Open **Settings → Providers**, enable **Grok**, and refresh. Set **Binary path** if `grok` isn't on the server's `PATH`.

- **Several accounts**: Give each provider instance its own `GROK_HOME` environment variable to keep logins and usage apart.
- **Updates**: Updates from Onicode follow how you installed Grok (the install script, Homebrew, or mise). Update a Nix install, or one Onicode cannot update, with its own package manager.

## Capabilities and limits

- **Models and options**: Onicode reads the models and reasoning effort levels from Grok Build. Add models it does not list with **Add custom model**.
- **Permission modes**: **Supervised**, **Auto** (Grok reviews routine actions and sends the rest to you), and **Full access**. Grok has no **Auto-accept edits** mode, so that mode runs as **Supervised**.
- **Skills**: Grok skills appear in the composer's `$` menu.
- **Usage and limits**: Subscription limits under **Usage → Limits** need `grok login`; they do not show with `XAI_API_KEY`.

## Troubleshooting

- **Provider check timeouts**: Startup checks in large directories can take a while. If they time out, set `ONICODE_GROK_PROBE_TIMEOUT_MS=60000` in the server's environment and restart ([Provider check timeouts](./install.md#provider-check-timeouts)).
