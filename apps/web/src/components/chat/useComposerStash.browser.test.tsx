import { EnvironmentId, PROVIDER_SEND_TURN_MAX_ATTACHMENTS, ThreadId } from "@onicode/contracts";
import { useEffect } from "react";
import { expect, it, vi } from "vite-plus/test";
import { page } from "vite-plus/test/browser";

import { act, render } from "../../../test/render";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { DraftId, useComposerDraftStore } from "../../composerDraftStore";
import { removeLocalStorageItem } from "../../hooks/useLocalStorage";
import { isInSideChatPane } from "../../lib/sideChatPane";
import { isMacPlatform } from "../../lib/utils";
import {
  PROMPT_STASH_STORAGE_KEY,
  usePromptStashStore,
  writePromptStashStorageForTest,
} from "../../promptStashStore";
import { BackgroundThreadContext } from "./backgroundThread";
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

it("matches reattach markers in the draft it restores into, not an open answer's", async () => {
  const target = DraftId.make("stash-answer-marker");
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
    Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 1 }, (_, index) => ({
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
  const notes = { name: "notes.txt", mimeType: "text/plain", sizeBytes: 5 };
  // An open question's answer draft holds a marker for the stashed file.
  const input = {
    ...stashInput(target, { current: "" }),
    readAttachmentDraft: () => ({
      images: [],
      files: [{ type: "file" as const, id: "marker", ...notes, file: null }],
    }),
  };
  usePromptStashStore.getState().stashEntry({
    id: "entry",
    createdAt: "2026-01-01T00:00:00.000Z",
    prompt: "Stashed",
    attachments: [],
    files: [
      { id: "notes", ...notes, attachmentId: "notes-upload", environmentId: input.environmentId },
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

  expect(store.getComposerDraft(target)?.files).toHaveLength(
    PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 1,
  );
});

it("stashes a side chat's prompt when Mod+S is pressed inside its pane", async () => {
  const target = DraftId.make("stash-side-chat");
  using _draft = {
    [Symbol.dispose]: () => useComposerDraftStore.getState().clearComposerContent(target),
  };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  const promptRef = { current: "Side chat question" };
  const baseInput = stashInput(target, promptRef);
  const input = {
    ...baseInput,
    routeThreadRef: {
      environmentId: baseInput.environmentId,
      threadId: ThreadId.make("stash-side-thread"),
    },
  } satisfies StashInput;
  // SideThreadPane renders an on-screen side chat this way.
  await using screen = await render(
    <BackgroundThreadContext value>
      <div data-side-chat-pane="stash-side-thread">
        <textarea aria-label="Side chat composer" />
        <Harness input={input} />
      </div>
    </BackgroundThreadContext>,
  );

  const composer = screen.container.querySelector("textarea")!;
  composer.focus();
  const mod = isMacPlatform(navigator.platform) ? { metaKey: true } : { ctrlKey: true };
  const event = new KeyboardEvent("keydown", { key: "s", ...mod, bubbles: true, cancelable: true });
  await act(() => {
    composer.dispatchEvent(event);
  });

  expect(event.defaultPrevented).toBe(true);
  await vi.waitFor(() => {
    expect(usePromptStashStore.getState().entries.map((entry) => entry.prompt)).toEqual([
      "Side chat question",
    ]);
  });
});

it("stashes the side chat's prompt when Mod+S is pressed inside its portaled popover", async () => {
  const mainTarget = DraftId.make("stash-main-chat-portal");
  const sideTarget = DraftId.make("stash-side-chat-portal");
  using _drafts = {
    [Symbol.dispose]: () => {
      const store = useComposerDraftStore.getState();
      store.clearComposerContent(mainTarget);
      store.clearComposerContent(sideTarget);
    },
  };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  const mainPromptRef = { current: "Main chat prompt" };
  const sidePromptRef = { current: "Side chat question" };
  const mainInput = stashInput(mainTarget, mainPromptRef);
  const sideBaseInput = stashInput(sideTarget, sidePromptRef);
  const sideInput = {
    ...sideBaseInput,
    routeThreadRef: {
      environmentId: sideBaseInput.environmentId,
      threadId: ThreadId.make("stash-side-portal-thread"),
    },
  } satisfies StashInput;

  await using screen = await render(
    <>
      <div id="stash-main-pane">
        <textarea id="stash-main-composer" aria-label="Main chat composer" />
        <Harness input={mainInput} />
        <Popover open>
          <PopoverTrigger>Branch</PopoverTrigger>
          <PopoverPopup>
            <input aria-label="Search branches" />
          </PopoverPopup>
        </Popover>
      </div>
      <BackgroundThreadContext value>
        <div data-side-chat-pane="stash-side-portal-thread">
          <Harness input={sideInput} />
          <Popover open>
            <PopoverTrigger>Model</PopoverTrigger>
            <PopoverPopup>
              <input aria-label="Search models" />
            </PopoverPopup>
          </Popover>
        </div>
      </BackgroundThreadContext>
    </>,
  );

  await expect.element(page.getByLabelText("Search models")).toBeVisible();
  await expect.element(page.getByLabelText("Search branches")).toBeVisible();
  const mainComposer = screen.getByLabelText("Main chat composer").element();
  const searchInput = page.getByLabelText("Search models").element() as HTMLInputElement;
  expect(isInSideChatPane(mainComposer)).toBe(false);
  expect(isInSideChatPane(page.getByLabelText("Search branches").element())).toBe(false);
  expect(isInSideChatPane(searchInput)).toBe(true);
  expect(searchInput.closest("[data-side-chat-pane]")).toBeNull();
  searchInput.focus();
  const mod = isMacPlatform(navigator.platform) ? { metaKey: true } : { ctrlKey: true };
  const event = new KeyboardEvent("keydown", { key: "s", ...mod, bubbles: true, cancelable: true });
  await act(() => {
    searchInput.dispatchEvent(event);
  });

  expect(event.defaultPrevented).toBe(true);
  await vi.waitFor(() => {
    expect(usePromptStashStore.getState().entries.map((entry) => entry.prompt)).toEqual([
      "Side chat question",
    ]);
  });
  expect(mainPromptRef.current).toBe("Main chat prompt");
  expect(sidePromptRef.current).toBe("");
});

function queuedEditInput(
  target: DraftId,
  promptRef: { current: string },
  editingQueuedMessage: NonNullable<StashInput["editingQueuedMessage"]>,
): StashInput {
  return { ...stashInput(target, promptRef), editingQueuedMessage };
}

function pressStashShortcut() {
  const mod = isMacPlatform(navigator.platform) ? { metaKey: true } : { ctrlKey: true };
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key: "s", ...mod, bubbles: true, cancelable: true }),
  );
}

it("moves an edited queued message to the stash and ends the edit", async () => {
  const target = DraftId.make("stash-queued-edit");
  using _draft = {
    [Symbol.dispose]: () => useComposerDraftStore.getState().clearComposerContent(target),
  };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  const promptRef = { current: "Queued follow-up, edited" };
  const onMovedToStash = vi.fn();
  await using _screen = await render(
    <Harness
      input={queuedEditInput(target, promptRef, { canMoveToStash: true, onMovedToStash })}
    />,
  );

  await act(pressStashShortcut);

  await vi.waitFor(() => {
    expect(usePromptStashStore.getState().entries.map((entry) => entry.prompt)).toEqual([
      "Queued follow-up, edited",
    ]);
  });
  expect(onMovedToStash).toHaveBeenCalledTimes(1);
  expect(promptRef.current).toBe("");
});

it("leaves an edited queued message alone when the stash cannot hold all of it", async () => {
  const target = DraftId.make("stash-queued-edit-attachments");
  using _draft = {
    [Symbol.dispose]: () => useComposerDraftStore.getState().clearComposerContent(target),
  };
  using _stash = {
    [Symbol.dispose]: () => {
      writePromptStashStorageForTest("");
      removeLocalStorageItem(PROMPT_STASH_STORAGE_KEY);
    },
  };
  const promptRef = { current: "Queued follow-up with a screenshot" };
  const onMovedToStash = vi.fn();
  await using _screen = await render(
    <Harness
      input={queuedEditInput(target, promptRef, { canMoveToStash: false, onMovedToStash })}
    />,
  );

  await act(pressStashShortcut);

  expect(usePromptStashStore.getState().entries).toEqual([]);
  expect(onMovedToStash).not.toHaveBeenCalled();
  expect(promptRef.current).toBe("Queued follow-up with a screenshot");
});
