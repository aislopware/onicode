import { EnvironmentId, ProjectId, ThreadId } from "@onicode/contracts";
import { scopeProjectRef, scopeThreadRef } from "@onicode/client-runtime/environment";
import { expect, it, vi } from "vite-plus/test";

import { DraftId, useComposerDraftStore } from "../composerDraftStore";
import { closeDraftThreadTerminals } from "./closeDraftThreadTerminals";
import { releaseProjectDrafts } from "./composerDraftUploads";

vi.mock("./attachmentUploadQueue", () => ({ releaseDraftAttachments: vi.fn() }));
vi.mock("./closeDraftThreadTerminals", () => ({ closeDraftThreadTerminals: vi.fn() }));

it("closes the terminals of a removed project's unsent draft threads", () => {
  const environmentId = EnvironmentId.make("environment-local");
  const projectRef = scopeProjectRef(environmentId, ProjectId.make("project-1"));
  const otherProjectRef = scopeProjectRef(environmentId, ProjectId.make("project-2"));
  const store = useComposerDraftStore.getState();
  store.setProjectDraftThreadId(projectRef, DraftId.make("draft-sent"), {
    threadId: ThreadId.make("thread-sent"),
  });
  store.setPrompt(DraftId.make("draft-sent"), "kept unmapped");
  store.markDraftThreadPromoting(
    DraftId.make("draft-sent"),
    scopeThreadRef(environmentId, ThreadId.make("thread-sent")),
  );
  store.setProjectDraftThreadId(projectRef, DraftId.make("draft-unsent"), {
    threadId: ThreadId.make("thread-unsent"),
  });
  store.setProjectDraftThreadId(otherProjectRef, DraftId.make("draft-other"), {
    threadId: ThreadId.make("thread-other"),
  });
  expect(store.getDraftSession(DraftId.make("draft-sent"))).not.toBeNull();

  releaseProjectDrafts(projectRef);

  expect(closeDraftThreadTerminals).toHaveBeenCalledExactlyOnceWith(
    environmentId,
    ThreadId.make("thread-unsent"),
  );
});
