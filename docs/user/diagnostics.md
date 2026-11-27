# Maintenance, power, and diagnostics

Control how much background checking Onicode does, keep your computer awake while agents work, inspect a server's health, and summarize its trace files.

## Background activity and power

These live under **Settings → Maintenance**, per environment:

- **Background activity**: Choose **Balanced**, **Performance**, or **Battery saver**, or open **Advanced** to choose when checks pause (host locked, low power mode, on battery) and set the **Git fetch interval** and provider check intervals.
- **Keep awake while agents work**: Keeps the host from sleeping while an agent turn runs.
- **Continue threads after restarts**: Resumes running threads after an update, a crash, or a reboot. See [Before you update](./updating.md#before-you-update).
- **Provider update checks**: Looks for newer provider CLIs and model lists.
- **Artifacts and logs** and **Delete now**: Clean up disk space ([Storage cleanup](./project-settings.md#storage-cleanup)) and compact the database ([Reclaim database space](./background-service.md#reclaim-database-space)).

## Diagnostics view

Open **Settings → Maintenance → Diagnostics** and choose an environment to see:

- Live server logs and recent failures
- Trace spans and CPU and memory history
- Running child processes, which you can stop from there
- **OpenTelemetry export**: send the server's traces, metrics, and logs to your own OTLP HTTP receiver. It applies after a server restart, and OTLP environment variables in the server's environment take precedence.

If the mobile app crashes while it starts, its **Diagnostics** settings list the startup crash on the next launch that succeeds, so you can copy it into a bug report ([details](./install.md#mobile-app)).

## Summarize trace files (`onicode trace`)

Summarize latency, rates, interrupts, and failures from the server's trace files in a terminal on the host:

```sh
onicode trace summary                    # The 25 busiest spans
onicode trace summary --since 30m        # Spans that ended in the last 30 minutes
onicode trace summary --since 2h --limit 50
```

Pass `--base-dir <path>` (or set `ONICODE_HOME`) for a data directory other than the default.
