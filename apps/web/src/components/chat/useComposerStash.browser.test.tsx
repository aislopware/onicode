import { EnvironmentId, PROVIDER_SEND_TURN_MAX_ATTACHMENTS, ThreadId } from "@onicode/contracts";
import { useEffect } from "react";
import { expect, it, vi } from "vite-plus/test";

import { act, render } from "../../../test/render";
import { DraftId, useComposerDraftStore } from "../../composerDraftStore";
import { removeLocalStorageItem } from "../../hooks/useLocalStorage";
import { isMacPlatform } from "../../lib/utils";
import {
  PROMPT_STASH_STORAGE_KEY,
  usePromptStashStore,
  writePromptStashStorageForTest,
} from "../../promptStashStore";
import {
  addPreparingComposerImages,
  countPreparingComposerImages,
  removePreparingComposerImages,
} from "./preparingComposerImages";
import { useComposerStash } from "./useComposerStash";

vi.mock("../../lib/attachmentUploadQueue", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/attachmentUploadQueue")>()),
  verifyStashedAttachmentUpload: async () => ({ status: "verified" }),
}));

type StashInput = Parameters<typeof useComposerStash>[0];
type Stash = ReturnType<typeof useComposerStash>;

function Harness({ input, ready }: { input: StashInput; ready?: (stash: Stash) => void }) {
  const stash = useComposerStash(input);
  useEffect(() => ready?.(stash));
  return null;
}

function stashInput(target: DraftId, promptRef: { current: string }) {
  const environmentId = EnvironmentId.make("stash-env");
  return {
    environmentId,
    composerDraftTarget: target,
    promptRef,
    readAttachmentDraft: () => ({ images: [], files: [] }),
    readTerminalContexts: () => [],
    routeThreadRef: { environmentId, threadId: ThreadId.make("stash-thread") },
    projectSelectionRequired: false,
    pendingUserInputs: [],
    activePendingProgress: null,
    prompt: promptRef.current,
    composerMenuOpen: false,
    isComposerCollapsedMobile: false,
    composerPreviewAnnotations: [],
    composerReviewComments: [],
    composerThreadContexts: [],
    attachmentTargetKey: target,
    isComposerDraftTargetOpen: () => true,
    composerEditorRef: { current: null },
    isRevertingCheckpoint: () => false,
    setComposerCursor: vi.fn(),
    setComposerTrigger: vi.fn(),
    importContextRecords: () => new Map(),
    expandMobileComposer: vi.fn(),
  } satisfies StashInput;
}

function preparingImage(id: string) {
  return { id, name: `${id}.png`, file: new File(["png"], `${id}.png`), hidden: false };
}

it("keeps the draft when stashing while a pasted image is still compressing", async () => {
  const target = DraftId.make("stash-preparing-image");
  using _draft = {
    [Symbol.dispose]: () => useComposerDraftStore.getState().clearComposerContent(target),
  };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  addPreparingComposerImages(target, [preparingImage("pasted")]);
  using _preparing = {
    [Symbol.dispose]: () => removePreparingComposerImages(target, new Set(["pasted"])),
  };
  const promptRef = { current: "Describe this screenshot" };
  const input = stashInput(target, promptRef);
  await using _screen = await render(<Harness input={input} />);

  const mod = isMacPlatform(navigator.platform) ? { metaKey: true } : { ctrlKey: true };
  await act(() => {
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", ...mod, bubbles: true, cancelable: true }),
    );
  });

  expect(usePromptStashStore.getState().entries).toEqual([]);
  expect(promptRef.current).toBe("Describe this screenshot");
  expect(countPreparingComposerImages(target)).toBe(1);
});

it("restores only into slots no pending attachment has reserved", async () => {
  const target = DraftId.make("stash-reserved-slot");
  const store = useComposerDraftStore.getState();
  using _draft = { [Symbol.dispose]: () => store.clearComposerContent(target) };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  store.addFiles(
    target,
    Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 3 }, (_, index) => ({
      type: "file" as const,
      id: `held-${index}`,
      name: `held-${index}.txt`,
      mimeType: "text/plain",
      sizeBytes: 1,
      file: new File(["x"], `held-${index}.txt`),
    })),
  );
  addPreparingComposerImages(target, [preparingImage("reserved")]);
  using _preparing = {
    [Symbol.dispose]: () => removePreparingComposerImages(target, new Set(["reserved"])),
  };
  const input = stashInput(target, { current: "" });
  const image = (id: string) => ({
    id,
    name: `${id}.png`,
    mimeType: "image/png",
    sizeBytes: 8,
    dataUrl: "data:image/png;base64,iVBORw0KGgo=",
  });
  usePromptStashStore.getState().stashEntry({
    id: "entry",
    createdAt: "2026-01-01T00:00:00.000Z",
    prompt: "Stashed",
    attachments: [image("first"), image("second")],
    files: [
      {
        id: "notes",
        name: "notes.txt",
        mimeType: "text/plain",
        sizeBytes: 5,
        attachmentId: "notes-upload",
        environmentId: input.environmentId,
      },
    ],
    droppedImageNames: [],
  });
  let stash!: Stash;
  await using _screen = await render(
    <Harness
      input={input}
      ready={(value) => {
        stash = value;
      }}
    />,
  );

  await act(() => stash.restoreStashEntryStable(usePromptStashStore.getState().entries[0]!));

  const draft = store.getComposerDraft(target);
  expect(draft?.files.at(-1)?.name).toBe("notes.txt");
  expect(draft?.images.map(({ name }) => name)).toEqual(["first.png"]);
});
