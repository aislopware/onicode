# Observability

Server observability has three outputs: pretty stdout logs (not persisted locally, except SSH-managed launches at `~/.onicode/ssh-launch/<state>/server.log`), a local NDJSON trace file (always on), and optional OTLP export (traces, metrics, logs).

## Local traces and event-loop stalls

Default trace paths (`server.trace.ndjson`):

- Production / explicit `--home-dir` / `ONICODE_HOME`: `<home>/userdata/logs/server.trace.ndjson`
- Linked worktree dev run: `<worktree>/.onicode/userdata/logs/server.trace.ndjson`
- Non-worktree dev run: `~/.onicode/dev/logs/server.trace.ndjson`

Summarize counts, rates, and latency percentiles offline (even while a server is stalled or stopped):

```sh
onicode trace summary --since 30m --limit 40
# Set ONICODE_TRACE_FILE=/path/to/server.trace.ndjson for dev or copied traces
```

Logs emitted inside an active span (`Effect.log*`) attach as span events unless `ONICODE_OTLP_LOGS_URL` is configured (which exports all log records directly to avoid duplicates).

Event-loop stalls (>2 s) write `server.eventLoop.stall` spans and append to `event-loop-stalls.ndjson` beside the trace file. SQLite spans carry `db.connection` (`writer`/`reader`) and `db.lock_wait_ms` (queue wait off the event loop—subtract it before blaming query speed). Metrics live only in-process unless OTLP export is enabled.

## Local OTLP stack (Grafana LGTM)

```sh
docker run --name lgtm -p 3000:3000 -p 4317:4317 -p 4318:4318 --rm -ti grafana/otel-lgtm
```

Open `http://localhost:3000` (`admin`/`admin`), export OTLP URLs in the shell, and restart the server from that same shell (packaged desktop apps launched from Finder/Dock/Start Menu do not inherit shell env vars):

```sh
export ONICODE_OTLP_TRACES_URL=http://localhost:4318/v1/traces
export ONICODE_OTLP_METRICS_URL=http://localhost:4318/v1/metrics
export ONICODE_OTLP_LOGS_URL=http://localhost:4318/v1/logs
export OTEL_RESOURCE_ATTRIBUTES=deployment.environment.name=development
vp run dev
```

## OTLP configuration and kill switches

- **Endpoints:** `ONICODE_OTLP_{TRACES,METRICS,LOGS}_URL` outrank standard `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` and `OTEL_EXPORTER_OTLP_ENDPOINT`. `ONICODE_OTLP_HEADERS` / `ONICODE_OTLP_PROTOCOL` (`http/json` default) apply only to `ONICODE_OTLP_*` endpoints; `OTEL_EXPORTER_OTLP_*` endpoints read `OTEL_EXPORTER_OTLP_*` headers/protocol (`http/protobuf` default). An endpoint set in Settings > Diagnostics ranks below both.
- **Service names are fixed:** `onicode-server` (backend), `onicode-desktop` (Electron main, traces/logs only), and `onicode-renderer` (UI), all carrying `onicode.release.channel` (`stable` or `dev`). `OTEL_SERVICE_NAME` is ignored—distinguish installs with `OTEL_RESOURCE_ATTRIBUTES` (e.g. `deployment.environment.name=staging`).
- **Kill switches:** `ONICODE_OTEL_SDK_DISABLED` (outranks `OTEL_SDK_DISABLED`, accepts standard boolean spellings; `OTEL_SDK_DISABLED` only accepts `true`) disables all OTLP export in server and desktop while keeping local traces and stdout intact. `OTEL_{TRACES,METRICS,LOGS}_EXPORTER=none` disables a single signal unless overridden by `ONICODE_OTLP_*_URL`.
- **Trace tuning:** `ONICODE_TRACE_FILE`, `ONICODE_TRACE_MAX_BYTES`, `ONICODE_TRACE_MAX_FILES`, `ONICODE_TRACE_MIN_LEVEL`, `ONICODE_TRACE_TIMING_ENABLED`.
- **Instrumentation rules:** Trace operation boundaries via `Effect.fn("Service.operation")` (see [Effect services](../internals/02_effect-services.md)). Put high-cardinality IDs/paths on span attributes, never on metric labels.

## Error reports (Sentry)

Release builds can send errors, a 5% sample of traces, and on-error screen replays to Sentry. The repository names no Sentry project: each DSN is inlined at build time (`ONICODE_SENTRY_{SERVER,DESKTOP,MOBILE}_DSN`, read by the server and desktop Vite configs and `apps/mobile/app.config.ts`), and a build without one never loads the SDK. Development runs (`vp run dev`, a development mobile variant, or any Metro-served bundle) send nothing unless `ONICODE_SENTRY_DEV=1`. `SENTRY_AUTH_TOKEN`, `SENTRY_URL`, and `SENTRY_ORG` enable source map uploads; a failed upload only warns.

- **Consent is per process owner** and on by default, applied on restart: the server's `observability.errorReporting`, the desktop's own `desktop-settings.json` (read before Electron is ready; the renderer reports through main), and a device-local switch on mobile. An unreadable settings file counts as off.
- **Narrowed at capture, not only scrubbed.** Logs contribute text parts only, span attributes only numbers and booleans, defects only `Error`s; payloads lose URL queries, fragments, and the home path (`packages/shared/src/sentryScrub.ts`).
- **Replay masking.** The renderer masks every attribute outside an allowlist, blocks links, frames, and media (rrweb records `href`/`src` before masking), and stops on routes whose address carries a query. The allowlist sets a private field of the replay integration; if a Sentry upgrade removes it, replays turn off.
- **Mobile plugin and fingerprint.** `@sentry/react-native/expo` is added only with a DSN, and the token's presence is a plugin prop, so the plan and build of a release must see the same environment.
- **Releases.** The release workflow's `sentry` job runs `scripts/release/sentry-release.sh` with `SENTRY_VCS_REMOTE` set to the GitHub repository; unset, Sentry gets a repository called `origin`.
- **Tracer bridge.** Effect spans reach Sentry through `packages/shared/src/sentry.ts`, not OTLP. A child of a span older than 30 seconds starts a new Sentry trace so one sampling decision cannot cover hours of work. Error and Fatal logs become handled issues; Info and Warn become breadcrumbs.

## Heap snapshots

On macOS and Linux (not Windows), send `SIGUSR2` to the server PID from `server-runtime.json` (in the server's state dir beside `logs/`) to write `<logsDir>/server-<pid>-<timestamp>.heapsnapshot` for Chrome DevTools Memory tab:

```sh
pid="$(jq .pid "${ONICODE_HOME:-$HOME/.onicode}/userdata/server-runtime.json")"
ps -p "$pid" -o command=   # verify it is the Onicode server—never signal desktop or launcher
kill -USR2 "$pid"
```

- Writing pauses the server (up to a minute+ on large heaps) and needs roughly as much free RAM as the heap—send once, and avoid on a swapping host.
- Snapshots contain tokens, secrets, and thread content. Never share them publicly, and delete them manually when finished.
