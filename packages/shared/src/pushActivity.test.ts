import { describe, expect, it } from "@effect/vitest";

import type { EnvironmentId, ThreadId } from "@onicode/contracts";
import { PushActivityState, PushNotification } from "@onicode/contracts/pushGateway";
import * as DateTime from "effect/DateTime";
import * as Schema from "effect/Schema";

import type { AgentAwarenessPhase, AgentAwarenessState } from "./agentAwareness.ts";
import {
  PUSH_TERMINAL_DISPLAY_MS,
  pushActivityIdentity,
  pushActivityState,
  pushAlertKind,
  pushNotificationForState,
} from "./pushActivity.ts";

const decodeActivityState = Schema.decodeUnknownOption(PushActivityState);
const decodeNotification = Schema.decodeUnknownOption(PushNotification);
const NOW = Date.parse("2026-11-12T12:00:00.000Z");
const at = (offsetMs: number) => DateTime.formatIso(DateTime.makeUnsafe(NOW + offsetMs));

const agent = (
  id: string,
  phase: AgentAwarenessPhase,
  updatedAt = at(0),
  threadTitle = `Thread ${id}`,
): AgentAwarenessState => ({
  environmentId: "env-1" as EnvironmentId,
  threadId: id as ThreadId,
  projectTitle: "onicode",
  threadTitle,
  phase,
  headline: "",
  modelTitle: "fable",
  updatedAt,
  deepLink: `/threads/env-1/${id}`,
});

const activity = (states: ReadonlyArray<AgentAwarenessState>, nowMs = NOW) =>
  pushActivityState({ title: "mac-mini", states, nowMs });

describe("pushActivityState", () => {
  it("orders by what needs someone, then work still going, then results that can wait", () => {
    const result = activity([
      agent("running", "running", at(-1_000)),
      agent("done", "completed", at(-60_000)),
      agent("approval", "waiting_for_approval", at(-5_000)),
      agent("failed", "failed", at(-2_000)),
      agent("stale", "stale", at(0)),
    ]);

    expect(result?.state.activities.map((row) => row.threadId)).toEqual([
      "approval",
      "failed",
      "running",
      "stale",
      "done",
    ]);
    expect(result?.state.activeCount).toBe(3);
    expect(result?.state.subtitle).toBe("3 agents working");
    expect(result?.expiresAt).toBeUndefined();
  });

  it("keeps working agents in place while they take turns", () => {
    const first = activity([agent("a", "running", at(-9_000)), agent("b", "starting", at(0))]);
    const second = activity([agent("a", "running", at(0)), agent("b", "running", at(-9_000))]);

    expect(first?.state.activities.map((row) => row.threadId)).toEqual(["a", "b"]);
    expect(second?.state.activities.map((row) => row.threadId)).toEqual(["a", "b"]);
  });

  it("keeps finished work for a while, then shows nothing", () => {
    const finished = [agent("done", "completed", at(-60_000))];

    const shown = activity(finished);
    expect(shown?.state.activeCount).toBe(0);
    expect(shown?.state.subtitle).toBe("Agent work completed");
    expect(shown?.expiresAt).toBe(NOW - 60_000 + PUSH_TERMINAL_DISPLAY_MS);

    expect(activity(finished, NOW + PUSH_TERMINAL_DISPLAY_MS)).toBeNull();
    expect(activity([])).toBeNull();
    // A result someone has opened waits for no one.
    expect(activity(finished.map((state) => ({ ...state, seen: true as const })))).toBeNull();
  });

  it("always fits the gateway's schema", () => {
    const titles = ["   ", "界".repeat(400), "🧑‍💻".repeat(70), `${"a".repeat(118)}😀b`];
    const states = Array.from({ length: 9 }, (_, index) =>
      agent(`t${index}`, "running", at(-index), titles[index] ?? "Thread"),
    );
    const result = activity(states);

    expect(decodeActivityState(result?.state)._tag).toBe("Some");
    expect(result?.state.activeCount).toBe(9);
    expect(result?.state.activities).toHaveLength(5);
    // The limit counts UTF-16 units, and a cut must not leave half an emoji.
    expect(result?.state.activities[3]?.threadTitle).toBe(`${"a".repeat(118)}…`);
  });

  it("counts a thread it cannot link to without showing it", () => {
    const delegated = `thread:delegated-task:${"x".repeat(120)}`;
    const unlinkable = "y".repeat(400);
    const result = activity([
      agent(delegated, "running", at(-1)),
      agent(unlinkable, "waiting_for_approval", at(0)),
    ]);

    expect(decodeActivityState(result?.state)._tag).toBe("Some");
    expect(result?.state.activities.map((row) => row.threadId)).toEqual([delegated]);
    expect(result?.state.activeCount).toBe(2);
    expect(pushNotificationForState(agent(unlinkable, "failed"), "digest")).toBeNull();
    expect(
      decodeNotification(
        pushNotificationForState(agent(delegated, "failed", at(0), "🧑‍💻".repeat(70)), "digest"),
      )._tag,
    ).toBe("Some");
  });

  it("has the same identity when only timestamps moved", () => {
    const before = activity([agent("a", "running", at(-9_000))]);
    const after = activity([agent("a", "running", at(0))]);
    const changed = activity([agent("a", "waiting_for_input", at(0))]);

    expect(pushActivityIdentity(after!.state)).toBe(pushActivityIdentity(before!.state));
    expect(pushActivityIdentity(changed!.state)).not.toBe(pushActivityIdentity(before!.state));
    expect(pushActivityIdentity(null)).not.toBe(pushActivityIdentity(before!.state));
  });
});

describe("pushAlertKind", () => {
  const kind = (
    previous: AgentAwarenessPhase | null,
    next: AgentAwarenessPhase,
    endedAgoMs: number | null = 1_000,
  ) =>
    pushAlertKind({
      previous,
      next,
      endedAtMs: endedAgoMs === null ? null : NOW - endedAgoMs,
      nowMs: NOW,
    });

  it.each([
    ["running", "waiting_for_approval", "approval"],
    [null, "waiting_for_input", "input"],
    ["waiting_for_approval", "waiting_for_input", "input"],
    ["running", "completed", "completion"],
    ["waiting_for_approval", "failed", "failure"],
    // A run short enough that nothing was shown before it ended.
    [null, "completed", "completion"],
    ["waiting_for_approval", "waiting_for_approval", null],
    ["waiting_for_approval", "running", null],
    ["starting", "running", null],
    ["completed", "failed", null],
  ] as const)("%s to %s alerts %s", (previous, next, expected) => {
    expect(kind(previous, next)).toBe(expected);
  });

  it("stays quiet about a result that did not just happen", () => {
    expect(kind(null, "completed", 3 * 60_000)).toBeNull();
    expect(kind("running", "failed", null)).toBeNull();
  });
});
