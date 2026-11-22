import { assert, it } from "@effect/vitest";

import {
  isNativeCommandMessage,
  leavesNotesForNextPrompt,
  notesWaitingMessageIds,
} from "./ContextHandoffDelivery.ts";

const user = (text: string) => ({ role: "user", text, attachments: [] });
const runsNothing = { runsAsNativeCommand: () => false };
const runsEverything = { runsAsNativeCommand: () => true };

it.each([
  { name: "a default command", session: {}, text: "/implement do X", native: true, wait: true },
  { name: "a path", session: {}, text: "/usr/lib/foo is broken", native: false, wait: false },
  // Older servers held notes for every leading `/name`, so history keeps holding them.
  {
    name: "a command sent as a prompt",
    session: runsNothing,
    text: "/review",
    native: false,
    wait: true,
  },
  // Onicode runs /compact itself; a compaction never carries a note or a missed image.
  { name: "a compaction", session: runsNothing, text: "/compact", native: false, wait: true },
  {
    name: "a namespaced command",
    session: { runsAsNativeCommand: () => true },
    text: "/team/review now",
    native: true,
    wait: true,
  },
  // Only `/` text is found again by the turn-start read, so nothing else may defer.
  {
    name: "a skill mention",
    session: runsEverything,
    text: "$deploy now",
    native: false,
    wait: false,
  },
])("classifies $name", ({ session, text, native, wait }) => {
  assert.equal(isNativeCommandMessage(session, user(text)), native);
  assert.equal(leavesNotesForNextPrompt(session, user(text)), wait);
});

it.each([
  { name: "an unsent command", accepted: false, steered: false, held: true },
  { name: "a command its provider already took", accepted: true, steered: false, held: false },
  { name: "an unsent command it steered away from", accepted: false, steered: true, held: false },
])("classifies a resume of $name by what it replays", ({ accepted, steered, held }) => {
  const source = { id: "run:source", userMessageId: "message:source" };
  const resume = {
    id: "run:resume",
    userMessageId: "message:resume",
    manualContinuationOfRunId: source.id,
  };
  const projection = {
    runs: [source, resume],
    attempts: [
      { id: "attempt:source", runId: source.id, attemptOrdinal: 1, reason: "initial" },
      ...(steered
        ? [{ id: "attempt:steer", runId: resume.id, attemptOrdinal: 2, reason: "steering_restart" }]
        : []),
    ],
    providerTurns: accepted ? [{ runAttemptId: "attempt:source" }] : [],
    messages: [{ id: source.userMessageId, ...user("/review") }],
  } as unknown as Parameters<typeof notesWaitingMessageIds>[1];
  assert.equal(notesWaitingMessageIds(runsNothing, projection).has(resume.userMessageId), held);
});
