import { expect, it } from "vite-plus/test";

import {
  compactionBlocksSteering,
  composerRunsDraftLocally,
  resolveComposerDispatchMode,
} from "./composerDispatch.ts";

it("blocks steering for explicit compaction, and for automatic compaction unless the provider takes steers then", () => {
  const during = (
    text: string | undefined,
    attachments: ReadonlyArray<unknown> = [],
    steersDuringCompaction = true,
  ) =>
    compactionBlocksSteering({
      compactCommandIsActive: false,
      compactionRunning: true,
      steersDuringCompaction,
      run: { userMessageId: "user" },
      messages: [
        ...(text === undefined ? [] : [{ id: "user", text, attachments }]),
        { id: "later", text: "/compact" },
      ],
    });

  for (const text of ["/compact", "  /COMPACT  ", "/compact keep the debugging notes"]) {
    expect(during(text)).toBe(true);
  }
  for (const text of ["Read the code", "/compactify", "please /compact", undefined]) {
    expect(during(text)).toBe(false);
  }
  expect(during("/compact", [{}])).toBe(false);
  expect(during("Read the code", [], false)).toBe(true);

  expect(
    compactionBlocksSteering({
      compactCommandIsActive: true,
      compactionRunning: false,
      steersDuringCompaction: true,
      run: undefined,
      messages: undefined,
    }),
  ).toBe(true);
  expect(
    compactionBlocksSteering({
      compactCommandIsActive: false,
      compactionRunning: false,
      steersDuringCompaction: false,
      run: { userMessageId: "user" },
      messages: [{ id: "user", text: "/compact" }],
    }),
  ).toBe(false);
});

it("treats only the commands the composer runs itself as local", () => {
  const none = {
    holdsOnlyText: true,
    sideChat: false,
    usageLimits: false,
    codexFeedback: false,
    freshSession: false,
    sessionViews: [],
  };
  const all = {
    ...none,
    sideChat: true,
    usageLimits: true,
    codexFeedback: true,
    freshSession: true,
    sessionViews: ["status" as const],
  };
  for (const text of ["/btw why?", "/usage-limits", "/feedback slow", "/clear", "/status"]) {
    expect(composerRunsDraftLocally({ ...all, text })).toBe(true);
    expect(composerRunsDraftLocally({ ...none, text })).toBe(false);
  }
  expect(composerRunsDraftLocally({ ...all, text: "fix the bug" })).toBe(false);
  expect(composerRunsDraftLocally({ ...all, holdsOnlyText: false, text: "/clear" })).toBe(false);
  expect(composerRunsDraftLocally({ ...all, holdsOnlyText: false, text: "/side why?" })).toBe(true);
});

it.each([
  { canSteer: true, editingQueuedRun: false, alternateModifier: true, mode: "steer" },
  // A turn that takes no steer queues either way.
  { canSteer: false, editingQueuedRun: false, alternateModifier: true, mode: "queue" },
  // A saved queued edit still asks for Send now, which moves a /compact to the front.
  { canSteer: false, editingQueuedRun: true, alternateModifier: true, mode: "steer" },
  { canSteer: false, editingQueuedRun: true, alternateModifier: false, mode: "queue" },
  // Sent after its screen left, the turn it saw may be over or replaced.
  {
    canSteer: true,
    editingQueuedRun: false,
    alternateModifier: true,
    sentAway: true,
    mode: "queue",
  },
] as const)(
  "dispatches $mode with canSteer $canSteer while editing a queued run: $editingQueuedRun",
  ({ mode, ...input }) => {
    expect(
      resolveComposerDispatchMode({ ...input, running: true, activeTurnDefault: "queue" }),
    ).toBe(mode);
  },
);
