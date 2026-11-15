import {
  ComposerContextId,
  EnvironmentId,
  type OrchestrationMessageContext,
  ThreadId,
} from "@onicode/contracts";
import {
  formatComposerContextReference,
  threadContextRecord,
} from "@onicode/shared/composerContextReferences";
import { describe, expect, it, vi } from "vite-plus/test";

import { environmentComposerContext } from "../lib/composerContext";
import { recallComposerPrompt } from "./composer-prompt-recall";
import {
  appendComposerDraftAttachments,
  captureComposerDraftInsertion,
  getComposerDraftSnapshot,
  setComposerDraftText,
} from "./use-composer-drafts";

vi.mock("expo-file-system", () => ({
  Directory: class {
    create() {}
  },
  File: class {
    exists = false;
    create() {}
    write() {}
    moveSync() {}
  },
  Paths: { document: { uri: "file:///documents" } },
}));

const environmentId = EnvironmentId.make("env-1");
const thread = threadContextRecord(
  { environmentId, threadId: ThreadId.make("thread-1") },
  "Correct invoice charge categories",
);
const machine = environmentComposerContext(environmentId, "Build box");
const mentioned = `Compare ${formatComposerContextReference(thread)} on ${formatComposerContextReference(machine)}`;
const context: OrchestrationMessageContext = {
  version: 1,
  records: [
    thread,
    machine,
    {
      version: 1,
      kind: "image",
      contextId: ComposerContextId.make("img-1"),
      label: "shot",
      attachmentId: "attachment-1",
      name: "shot.png",
      mimeType: "image/png",
      sizeBytes: 10,
    },
  ],
};
const messages = [
  { id: "m1", role: "user", text: "first" },
  { id: "a1", role: "assistant", text: "reply" },
  { id: "m2", role: "user", text: `${mentioned} ![shot](oni-context://v1/image/img-1)`, context },
];

describe("recallComposerPrompt", () => {
  it("walks the thread's prompts and restores thread and machine mentions", () => {
    const draftKey = "env-1:recall-walk";
    const step = (direction: "backward" | "forward") =>
      recallComposerPrompt({ draftKey, direction, messages });

    step("backward");
    expect(getComposerDraftSnapshot(draftKey)).toMatchObject({
      text: mentioned,
      context: { version: 1, records: [thread, machine] },
    });
    expect(captureComposerDraftInsertion(draftKey)).toEqual({
      text: mentioned,
      start: mentioned.length,
      end: mentioned.length,
    });

    step("backward");
    expect(getComposerDraftSnapshot(draftKey)).toMatchObject({ text: "first", context: undefined });
    step("forward");
    expect(getComposerDraftSnapshot(draftKey).context?.records).toEqual([thread, machine]);
    step("forward");
    expect(getComposerDraftSnapshot(draftKey).text).toBe("");
  });

  it("leaves a draft the user is composing alone", () => {
    const typedKey = "env-1:recall-typed";
    setComposerDraftText(typedKey, "half typed");
    recallComposerPrompt({ draftKey: typedKey, direction: "backward", messages });
    expect(getComposerDraftSnapshot(typedKey).text).toBe("half typed");

    const editedKey = "env-1:recall-edited";
    recallComposerPrompt({ draftKey: editedKey, direction: "backward", messages });
    setComposerDraftText(editedKey, `${mentioned}!`);
    recallComposerPrompt({ draftKey: editedKey, direction: "backward", messages });
    expect(getComposerDraftSnapshot(editedKey).text).toBe(`${mentioned}!`);

    const attachedKey = "env-1:recall-attached";
    appendComposerDraftAttachments(attachedKey, [
      {
        type: "file",
        id: "file",
        name: "notes.txt",
        mimeType: "text/plain",
        sizeBytes: 4,
        fileUri: "file:///notes.txt",
      },
    ]);
    recallComposerPrompt({ draftKey: attachedKey, direction: "backward", messages });
    expect(getComposerDraftSnapshot(attachedKey).text).toBe("");
  });
});
