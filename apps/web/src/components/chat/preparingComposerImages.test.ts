import { scopeProjectRef, scopeThreadRef } from "@onicode/client-runtime/environment";
import { EnvironmentId, ProjectId, ProviderDriverKind, ThreadId } from "@onicode/contracts";
import { expect, it } from "vite-plus/test";

import { composerTargetKey, DraftId, useComposerDraftStore } from "../../composerDraftStore";
import {
  addPreparingComposerImages,
  cancelHeldComposerSend,
  cancelPreparingComposerImages,
  holdComposerSend,
  joinPreparedImagesToDraft,
  removePreparingComposerImages,
  watchComposerSendEdits,
} from "./preparingComposerImages";

function preparingImage(id: string) {
  return { id, name: `${id}.png`, file: new File(["png"], `${id}.png`), hidden: false };
}

it("holds a message send until its images join the draft, and cancels on edit, cancel, or new paste", async () => {
  const draftTarget = scopeThreadRef(EnvironmentId.make("environment-held"), ThreadId.make("held"));
  const target = composerTargetKey(draftTarget);
  const drafts = useComposerDraftStore.getState();
  using _store = {
    [Symbol.dispose]: () => {
      cancelPreparingComposerImages(target);
      drafts.clearComposerContent(draftTarget);
    },
  };
  drafts.setPrompt(draftTarget, "check");
  expect(holdComposerSend(draftTarget)).toBeNull();

  // Two pastes compress separately; the first joining the draft is not an edit.
  addPreparingComposerImages(target, [preparingImage("first")]);
  addPreparingComposerImages(target, [preparingImage("second")]);
  const held = holdComposerSend(draftTarget);
  await expect(holdComposerSend(draftTarget)).resolves.toEqual({ status: "cancelled" });
  removePreparingComposerImages(target, new Set(["first"]));
  joinPreparedImagesToDraft(() => drafts.setPrompt(draftTarget, "check first"));
  removePreparingComposerImages(target, new Set(["second"]));
  await expect(held).resolves.toEqual({ status: "ready" });

  for (const cancel of [
    () => drafts.setPrompt(draftTarget, "check again"),
    () =>
      drafts.setProviderModelOptions(draftTarget, ProviderDriverKind.make("codex"), [
        { id: "reasoningEffort", value: "high" },
      ]),
    () => cancelHeldComposerSend(target),
    () => addPreparingComposerImages(target, [preparingImage("added-mid-wait")]),
    () => removePreparingComposerImages(target, new Set(["later"]), { cancelSubmission: true }),
  ]) {
    addPreparingComposerImages(target, [preparingImage("later")]);
    const cancelled = holdComposerSend(draftTarget);
    cancel();
    await expect(cancelled).resolves.toEqual({ status: "cancelled" });
    cancelPreparingComposerImages(target);
  }
});

it("cancels a held draft send when the draft moves machine or a new worktree's base, not when a local branch syncs", async () => {
  const draftId = DraftId.make("draft-held-place");
  const target = composerTargetKey(draftId);
  const drafts = useComposerDraftStore.getState();
  const projectA = scopeProjectRef(EnvironmentId.make("environment-a"), ProjectId.make("project"));
  const projectB = scopeProjectRef(EnvironmentId.make("environment-b"), ProjectId.make("project"));
  using _store = {
    [Symbol.dispose]: () => {
      cancelPreparingComposerImages(target);
      drafts.clearProjectDraftThreadById(projectA, draftId);
      drafts.clearProjectDraftThreadById(projectB, draftId);
    },
  };
  drafts.setProjectDraftThreadId(projectA, draftId, { threadId: ThreadId.make("held-place") });
  drafts.setPrompt(draftId, "check");

  addPreparingComposerImages(target, [preparingImage("local")]);
  const kept = holdComposerSend(draftId);
  drafts.setDraftThreadContext(draftId, { branch: "synced-checkout" });
  removePreparingComposerImages(target, new Set(["local"]));
  await expect(kept).resolves.toEqual({ status: "ready" });

  for (const move of [
    () => drafts.setDraftThreadContext(draftId, { projectRef: projectB }),
    () => drafts.setDraftThreadContext(draftId, { envMode: "worktree", branch: "release" }),
  ]) {
    addPreparingComposerImages(target, [preparingImage("moved")]);
    const cancelled = holdComposerSend(draftId);
    move();
    await expect(cancelled).resolves.toEqual({ status: "cancelled" });
    cancelPreparingComposerImages(target);
  }
});

it("distinguishes draft edits from attachment progress while routing a send", () => {
  const environmentId = EnvironmentId.make("environment-routing");
  const draftTarget = scopeThreadRef(environmentId, ThreadId.make("routing"));
  const target = composerTargetKey(draftTarget);
  const drafts = useComposerDraftStore.getState();
  using _store = {
    [Symbol.dispose]: () => {
      cancelPreparingComposerImages(target);
      drafts.clearComposerContent(draftTarget);
    },
  };
  const file = new File(["text"], "notes.txt", { type: "text/plain" });
  drafts.setPrompt(draftTarget, "read these");
  const setEffort = (value: string) =>
    drafts.setProviderModelOptions(draftTarget, ProviderDriverKind.make("codex"), [
      { id: "reasoningEffort", value },
    ]);
  setEffort("medium");
  const finishSameEffortSend = watchComposerSendEdits(draftTarget);
  setEffort("medium");
  expect(finishSameEffortSend()).toBe("ready");

  drafts.addFiles(draftTarget, [
    { type: "file", id: "notes", name: file.name, mimeType: file.type, sizeBytes: file.size, file },
  ]);
  const image = preparingImage("submitted");
  addPreparingComposerImages(target, [image]);
  const finishUnchangedSend = watchComposerSendEdits(draftTarget);
  drafts.setPrompt(draftTarget, drafts.getComposerDraft(draftTarget)!.prompt);
  drafts.setFileUpload(draftTarget, "notes", environmentId, "uploaded-notes");
  joinPreparedImagesToDraft(() =>
    drafts.addImages(draftTarget, [
      {
        type: "image",
        id: image.id,
        name: image.name,
        mimeType: "image/png",
        sizeBytes: image.file.size,
        previewUrl: URL.createObjectURL(image.file),
        file: image.file,
      },
    ]),
  );
  removePreparingComposerImages(target, new Set([image.id]));
  expect(finishUnchangedSend()).toBe("ready");

  for (const edit of [
    () => drafts.setPrompt(draftTarget, "replacement"),
    () => setEffort("high"),
    () => addPreparingComposerImages(target, [preparingImage("unsubmitted")]),
  ]) {
    const finishEditedSend = watchComposerSendEdits(draftTarget);
    edit();
    expect(finishEditedSend()).toBe("edited");
  }

  const finishCancelledSend = watchComposerSendEdits(draftTarget);
  drafts.setPrompt(draftTarget, "another edit");
  removePreparingComposerImages(target, new Set(["unsubmitted"]), { cancelSubmission: true });
  expect(finishCancelledSend()).toBe("attachments-cancelled");
});
