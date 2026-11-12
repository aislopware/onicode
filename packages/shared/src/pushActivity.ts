import type { PushDevicePreferences } from "@onicode/contracts";
import {
  PUSH_ACTIVITY_MAX_ROWS,
  PushActivityRow,
  type PushActivityState,
  PushNotification,
} from "@onicode/contracts/pushGateway";
import * as Schema from "effect/Schema";

import type { AgentAwarenessPhase, AgentAwarenessState } from "./agentAwareness.ts";

/** How long finished work stays on the Live Activity and the ongoing notification. */
export const PUSH_TERMINAL_DISPLAY_MS = 15 * 60_000;
/**
 * How often a Live Activity with work under way is sent again unchanged. The gateway tells iOS a
 * card is out of date 10 minutes after its last update, so this stays under that.
 */
export const PUSH_ACTIVITY_KEEPALIVE_MS = 8 * 60_000;
export const PUSH_ACTIVITY_KEEPALIVE_RETRY_MS = 60_000;
/** A run that ended longer ago than this is history, not news. */
const PUSH_TERMINAL_ALERT_FRESH_MS = 2 * 60_000;
const TEXT_MAX_LENGTH = 120;
const BODY_MAX_LENGTH = 240;

// A thread whose id or link is past the gateway's limits cannot be shown, and must not sink the rest.
const isRow = Schema.is(PushActivityRow);
const isNotification = Schema.is(PushNotification);

export type PushAlertKind = "approval" | "input" | "completion" | "failure";

const isTerminal = (phase: AgentAwarenessPhase) => phase === "completed" || phase === "failed";
const isAttention = (phase: AgentAwarenessPhase) =>
  phase === "waiting_for_approval" || phase === "waiting_for_input";

function statusForPhase(phase: AgentAwarenessPhase): string {
  switch (phase) {
    case "waiting_for_approval":
      return "Approval";
    case "waiting_for_input":
      return "Input";
    case "completed":
      return "Done";
    case "failed":
      return "Failed";
    case "starting":
      return "Connecting";
    case "running":
      return "Working";
    case "stale":
      return "Waiting";
  }
}

const isInFlight = (phase: AgentAwarenessPhase) =>
  phase === "starting" || phase === "running" || phase === "stale";

/** What someone has to act on or look at comes before what is still going. */
function phasePriority(phase: AgentAwarenessPhase): number {
  if (isAttention(phase)) return 0;
  if (phase === "failed") return 1;
  if (phase === "completed") return 2;
  if (phase === "stale") return 4;
  return 3;
}

function fit(value: string, fallback: string, maxLength = TEXT_MAX_LENGTH): string {
  const text = value.replace(/\s+/g, " ").trim();
  if (text.length === 0) return fallback;
  if (text.length <= maxLength) return text;
  // The gateway counts UTF-16 units, so cut there, stepping back from half a surrogate pair.
  let end = maxLength - 1;
  const last = text.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end -= 1;
  return `${text.slice(0, end).trimEnd()}…`;
}

function row(state: AgentAwarenessState): PushActivityRow {
  return {
    environmentId: state.environmentId,
    threadId: state.threadId,
    projectTitle: fit(state.projectTitle, "Project"),
    threadTitle: fit(state.threadTitle, "Untitled thread"),
    modelTitle: fit(state.modelTitle, "Agent"),
    phase: state.phase,
    status: statusForPhase(state.phase),
    updatedAt: state.updatedAt,
    deepLink: state.deepLink,
  };
}

const newestFirst = (a: AgentAwarenessState, b: AgentAwarenessState) =>
  b.updatedAt.localeCompare(a.updatedAt);

// A working thread's timestamp moves with every step. Ordering those by it would swap rows, and
// push an update, each time two agents took turns.
const displayOrder = (a: AgentAwarenessState, b: AgentAwarenessState) =>
  phasePriority(a.phase) - phasePriority(b.phase) ||
  (isInFlight(a.phase) ? a.threadId.localeCompare(b.threadId) : newestFirst(a, b));

/**
 * One environment's agents as a phone shows them, or null when there is nothing worth showing.
 * `expiresAt` is when finished work stops being worth showing with nothing running.
 */
export function pushActivityState(input: {
  readonly title: string;
  readonly states: ReadonlyArray<AgentAwarenessState>;
  readonly nowMs: number;
}): { readonly state: PushActivityState; readonly expiresAt: number | undefined } | null {
  const active = input.states.filter((state) => !isTerminal(state.phase));
  const finished = input.states
    .filter(
      (state) =>
        isTerminal(state.phase) &&
        input.nowMs - Date.parse(state.updatedAt) <= PUSH_TERMINAL_DISPLAY_MS,
    )
    .sort(newestFirst);
  const shown = [...active, ...finished].sort(displayOrder);
  const newest = [...shown].sort(newestFirst)[0];
  if (newest === undefined) return null;

  const count = active.length;
  return {
    state: {
      title: fit(input.title, "Onicode"),
      subtitle:
        count > 0
          ? `${count} agent${count === 1 ? "" : "s"} working`
          : finished[0]?.phase === "failed"
            ? "Agent work failed"
            : "Agent work completed",
      activeCount: Math.min(count, 9999),
      updatedAt: newest.updatedAt,
      activities: shown.map(row).filter(isRow).slice(0, PUSH_ACTIVITY_MAX_ROWS),
    },
    expiresAt:
      count > 0 || finished[0] === undefined
        ? undefined
        : Date.parse(finished[0].updatedAt) + PUSH_TERMINAL_DISPLAY_MS,
  };
}

/** What changes on screen; `updatedAt` moves on every event and must not count. */
export function pushActivityIdentity(state: PushActivityState | null): string {
  if (state === null) return "null";
  return JSON.stringify({
    ...state,
    updatedAt: undefined,
    activities: state.activities.map((activity) => ({ ...activity, updatedAt: undefined })),
  });
}

/**
 * Whether a thread's move from one phase to another is worth interrupting someone for.
 * `previous` is null for a thread that had nothing to show.
 */
export function pushAlertKind(input: {
  readonly previous: AgentAwarenessPhase | null;
  readonly next: AgentAwarenessPhase;
  /** When the thread's latest run ended, for terminal phases. */
  readonly endedAtMs: number | null;
  readonly nowMs: number;
}): PushAlertKind | null {
  const { previous, next } = input;
  if (previous === next) return null;
  if (next === "waiting_for_approval") return "approval";
  if (next === "waiting_for_input") return "input";
  if (!isTerminal(next) || (previous !== null && isTerminal(previous))) return null;
  // Unarchiving or unsettling an old result brings it back without anything having just ended.
  if (input.endedAtMs === null || input.nowMs - input.endedAtMs > PUSH_TERMINAL_ALERT_FRESH_MS) {
    return null;
  }
  return next === "failed" ? "failure" : "completion";
}

export function pushAlertAllowed(preferences: PushDevicePreferences, kind: PushAlertKind): boolean {
  switch (kind) {
    case "approval":
      return preferences.notifyOnApproval;
    case "input":
      return preferences.notifyOnInput;
    case "completion":
      return preferences.notifyOnCompletion;
    case "failure":
      return preferences.notifyOnFailure;
  }
}

/** `id` names this one alert in at most 64 base64url characters. Null when the thread cannot be linked to. */
export function pushNotificationForState(
  state: AgentAwarenessState,
  id: string,
): PushNotification | null {
  const notification = {
    id,
    title: fit(state.threadTitle, "Untitled thread"),
    body: fit(
      `${state.headline} · ${fit(state.projectTitle, "Project")}`,
      "Agent update",
      BODY_MAX_LENGTH,
    ),
    environmentId: state.environmentId,
    threadId: state.threadId,
    deepLink: state.deepLink,
  };
  return isNotification(notification) ? notification : null;
}
