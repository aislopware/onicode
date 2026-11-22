import { assert, it } from "@effect/vitest";

import { isNativeCommandMessage, leavesNotesForNextPrompt } from "./ContextHandoffDelivery.ts";

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
