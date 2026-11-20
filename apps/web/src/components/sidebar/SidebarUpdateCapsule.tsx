import { useAtomValue } from "@effect/atom-react";
import { msg, T, useGT, useMessages } from "gt-react";
import {
  AppWindowIcon,
  ArrowUpIcon,
  ClockAlertIcon,
  DownloadIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { useState, useSyncExternalStore, type ReactNode } from "react";

import type { MessageResolver } from "~/i18n/messages";
import { useMediaQuery } from "~/hooks/useMediaQuery";
import { cn } from "~/lib/utils";
import { environmentIdsKey } from "~/state/environmentAtomList";
import { useEnvironmentMachines } from "~/state/environments";
import { environmentUpdateStatesAtom } from "~/state/server";
import { EnvironmentMachineIcon } from "../EnvironmentMachineIcon";
import { useServerUpdate } from "../ServerUpdateAction";
import { ThreadHoverCard } from "../ThreadHoverCard";
import { Button } from "../ui/button";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { Spinner } from "../ui/spinner";
import { UpdateProgressRing } from "./DesktopUpdateStatusIcon";
import {
  describeUpdateCapsule,
  formatUpdateElapsed,
  RUN_STATUS_LABELS,
  resolveUpdateRunMachineStatus,
  UPDATE_RESTART_DELAY_MS,
  type UpdateCapsuleView,
  type UpdateEverythingRun,
  type UpdateRunLocalState,
  type UpdateRunMachineView,
} from "./updateEverything.logic";
import {
  dismissSettledUpdateRunMachines,
  postponeUpdateRestart,
  retryFailedUpdateRunServers,
} from "./updateEverythingRun";

/** Steps count from `origin`, so a countdown to it changes on its own second boundaries. */
function useNow(intervalMs: number | null, origin: number): number {
  const step = intervalMs ?? 60_000;
  const subscribe = (onChange: () => void) => {
    if (intervalMs === null) return () => {};
    const timer = setInterval(onChange, intervalMs);
    return () => clearInterval(timer);
  };
  return useSyncExternalStore(
    subscribe,
    () => Math.floor((Date.now() - origin) / step) * step + origin,
  );
}

function useUpdateRunMachineViews(run: UpdateEverythingRun): ReadonlyArray<UpdateRunMachineView> {
  const updateStates = useAtomValue(
    environmentUpdateStatesAtom(
      environmentIdsKey(run.machines.map(({ environmentId }) => environmentId)),
    ),
  );
  return run.machines.map((machine, index) => ({
    machine,
    status: resolveUpdateRunMachineStatus(machine, run.phase, updateStates[index]!),
  }));
}

function restartSecondsLeft(run: UpdateEverythingRun, now: number): number | null {
  return run.phase === "countdown" && run.restartAt !== null
    ? Math.ceil(Math.min(UPDATE_RESTART_DELAY_MS, Math.max(0, run.restartAt - now)) / 1000)
    : null;
}

function localStatusLabel(
  run: UpdateEverythingRun,
  local: UpdateRunLocalState,
  now: number,
  resolve: MessageResolver,
) {
  const seconds = restartSecondsLeft(run, now);
  if (seconds !== null) return resolve(msg("restarts in {seconds}s", { seconds }));
  if (run.phase === "installing") return resolve(msg("restarting"));
  if (local?.status === "downloading") {
    return local.percent === null
      ? resolve(msg("downloading"))
      : resolve(msg("downloading {percent}%", { percent: Math.floor(local.percent) }));
  }
  return resolve(msg("restarts after the servers"));
}

function UpdateCapsuleGlyph({
  view,
  restartAt,
  restartSeconds,
}: {
  readonly view: UpdateCapsuleView;
  readonly restartAt: number | null;
  readonly restartSeconds: number | null;
}) {
  const m = useMessages();
  const reducedMotion = useMediaQuery("(prefers-reduced-motion: reduce)");
  // Read once per countdown: the ring then drains by itself until the restart.
  const [drain] = useState(() => ({
    from: view.progress,
    ms: restartAt === null ? 0 : Math.max(0, restartAt - Date.now()),
  }));

  if (view.glyph === "restarting") return <Spinner size="md" />;
  if (view.glyph === "countdown") {
    return (
      <UpdateProgressRing
        fraction={reducedMotion ? view.progress : drain.from}
        drainMs={reducedMotion ? undefined : drain.ms}
      >
        <span className="text-xs font-medium tabular-nums">
          {m(msg("{seconds}s", { seconds: restartSeconds ?? 0 }))}
        </span>
      </UpdateProgressRing>
    );
  }
  return (
    <UpdateProgressRing fraction={view.progress}>
      {view.glyph === "failed" ? (
        <TriangleAlertIcon className="size-4" />
      ) : view.glyph === "downloading" ? (
        <DownloadIcon className="size-4" />
      ) : view.tone === "slow" ? (
        <ClockAlertIcon className="size-4" />
      ) : (
        <ArrowUpIcon className="size-4" />
      )}
    </UpdateProgressRing>
  );
}

/** A machine and where its update stands; only trouble takes a colour. */
function UpdateRunRow({
  icon,
  name,
  status,
  elapsed,
  tone,
  detail,
}: {
  readonly icon: ReactNode;
  readonly name: ReactNode;
  readonly status: string;
  readonly elapsed?: string | undefined;
  readonly tone?: "slow" | "failed" | undefined;
  readonly detail?: string | null;
}) {
  return (
    <div className={detail ? "min-w-56" : "min-w-0"}>
      <div className="flex min-w-0 items-center gap-2">
        {icon}
        <div className="min-w-0 flex-1 truncate text-foreground/80">{name}</div>
        <div
          className={cn(
            "flex shrink-0 gap-1.5 pl-4 tabular-nums",
            tone === "failed" && "text-error",
            tone === "slow" && "text-warning",
          )}
        >
          <span>{status}</span>
          {elapsed ? <span>{elapsed}</span> : null}
        </div>
      </div>
      {/* Zero intrinsic width: the rows size the popup and a long failure wraps inside it. */}
      {detail ? <div className="mt-0.5 w-0 min-w-full pl-5 wrap-anywhere">{detail}</div> : null}
    </div>
  );
}

export function SidebarUpdateCapsule({
  run,
  local,
  onRestart,
}: {
  readonly run: UpdateEverythingRun;
  readonly local: UpdateRunLocalState;
  readonly onRestart?: () => Promise<void>;
}) {
  const machines = useUpdateRunMachineViews(run);
  const hasActiveMachine = machines.some(
    ({ status }) => status !== "done" && status !== "failed" && status !== "waiting",
  );
  const now = useNow(
    run.phase === "countdown" ? 1_000 : hasActiveMachine ? 30_000 : null,
    run.restartAt ?? 0,
  );
  const gt = useGT();
  const m = useMessages();
  const view = describeUpdateCapsule(run, machines, local, now, m);
  const updateServer = useServerUpdate();
  const failed = machines.some(({ status }) => status === "failed");
  const settled = !hasActiveMachine && run.localVersion === null;
  const restartSeconds = restartSecondsLeft(run, now);
  const machineById = useEnvironmentMachines();
  // The title already says what this app is doing in these states, so its row would repeat it.
  const showLocalRow =
    run.localVersion !== null &&
    view.glyph !== "countdown" &&
    view.glyph !== "restarting" &&
    view.glyph !== "downloading";
  const restart = run.localVersion === null ? onRestart : undefined;
  // The restart is seconds away and "Later" lives in the popup, so the countdown brings it up.
  const [open, setOpen] = useState(run.phase === "countdown");
  const [phaseSeen, setPhaseSeen] = useState(run.phase);
  if (phaseSeen !== run.phase) {
    setPhaseSeen(run.phase);
    if (run.phase === "countdown") setOpen(true);
  }

  const headline = view.slow
    ? gt("{summary}. {machine}: {status}, {elapsed}", {
        summary: view.title,
        machine: view.label,
        status: view.slow.status,
        elapsed: view.slow.elapsed,
      })
    : view.label;
  // Said once per step: a countdown or a percentage read out every tick would drown the reader.
  const announcement =
    view.glyph === "countdown"
      ? gt("Restart in {seconds}s", { seconds: UPDATE_RESTART_DELAY_MS / 1000 })
      : view.glyph === "downloading"
        ? gt("Downloading")
        : headline;
  const actionLabel =
    view.action === "later"
      ? gt("Restart later")
      : view.action === "retry"
        ? gt("Retry")
        : gt("Dismiss");
  const runAction = () => {
    if (view.action === "later") postponeUpdateRestart();
    else if (view.action === "retry") retryFailedUpdateRunServers(updateServer);
    else dismissSettledUpdateRunMachines();
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next, details) => {
        // "Later" has seconds to be found: the pointer leaving does not take it away.
        if (!next && run.phase === "countdown" && details.reason === "trigger-hover") return;
        setOpen(next);
      }}
    >
      <PopoverTrigger
        openOnHover
        closeDelay={150}
        render={
          <button
            type="button"
            aria-label={headline}
            className={cn(
              "grid size-8 shrink-0 cursor-pointer place-items-center rounded-full bg-sidebar-control-surface outline-hidden ring-ring transition-colors hover:bg-sidebar-row-hover focus-visible:ring-2 focus-visible:ring-inset active:bg-sidebar-row-active data-popup-open:bg-sidebar-row-hover",
              view.tone === "slow"
                ? "text-warning"
                : view.tone === "failed"
                  ? "text-destructive"
                  : "text-sidebar-foreground",
            )}
          />
        }
      >
        {/* Keyed by state, so each new glyph fades in instead of snapping over the last. */}
        <span
          key={`${view.glyph}:${view.tone}`}
          className="grid place-items-center transition-[opacity,scale] duration-200 ease-out-quint starting:opacity-0 motion-safe:starting:scale-[0.97]"
        >
          <UpdateCapsuleGlyph
            key={run.restartAt}
            view={view}
            restartAt={run.restartAt}
            restartSeconds={restartSeconds}
          />
        </span>
      </PopoverTrigger>
      {/* Outside the button: a button's own label hides what it contains from screen readers. */}
      <span role="status" className="sr-only">
        {announcement}
      </span>
      {/* A popover's radius, not a tooltip's: the buttons inside share the tooltip's. */}
      <PopoverPopup side="top" align="end" padding="none" initialFocus={false}>
        <div className="px-2 py-1">
          <ThreadHoverCard
            // Digits only: tabular figures also widen the hyphens in machine names.
            title={<span className="tabular-nums">{view.title}</span>}
            footer={
              restart || view.action !== null ? (
                <div className="flex flex-wrap gap-1.5 *:flex-1">
                  {restart ? (
                    <Button size="xs" onClick={() => void restart()}>
                      <T>Restart to update</T>
                    </Button>
                  ) : null}
                  {settled && failed && view.action === "retry" ? (
                    <Button size="xs" variant="ghost" onClick={dismissSettledUpdateRunMachines}>
                      <T>Dismiss</T>
                    </Button>
                  ) : null}
                  {view.action !== null ? (
                    <Button size="xs" variant="outline" onClick={runAction}>
                      {actionLabel}
                    </Button>
                  ) : null}
                </div>
              ) : null
            }
          >
            {showLocalRow ? (
              <UpdateRunRow
                icon={<AppWindowIcon className="size-3 shrink-0 stroke-muted-foreground" />}
                name={<T>This app</T>}
                status={localStatusLabel(run, local, now, m)}
              />
            ) : null}
            {machines.map(({ machine, status }) => (
              <UpdateRunRow
                key={machine.environmentId}
                icon={
                  <EnvironmentMachineIcon
                    kind={machineById.get(machine.environmentId) ?? "server"}
                    className="size-3 shrink-0 stroke-muted-foreground"
                  />
                }
                name={machine.label}
                status={m(RUN_STATUS_LABELS[status])}
                elapsed={
                  status === "done" || status === "failed" || status === "waiting"
                    ? undefined
                    : formatUpdateElapsed(now - machine.startedAt, m)
                }
                tone={
                  status === "failed"
                    ? "failed"
                    : view.slow?.environmentId === machine.environmentId
                      ? "slow"
                      : undefined
                }
                detail={machine.failure}
              />
            ))}
          </ThreadHoverCard>
        </div>
      </PopoverPopup>
    </Popover>
  );
}
