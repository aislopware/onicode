import { threadContextRecord } from "@onicode/shared/composerContextReferences";
import { useEffect } from "react";
import {
  ChatAttachmentId,
  type ComposerContextRecord,
  ComposerContextId,
  EnvironmentId,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
  RuntimeRequestId,
  ThreadId,
} from "@onicode/contracts";
import { scopeThreadRef } from "@onicode/client-runtime/environment";
import { makeThreadRequestsResolvedAnnouncer } from "@onicode/client-runtime/state/thread-requests";
import { expect, it, vi } from "vite-plus/test";

import { act, render, stubGlobal } from "../../../test/render";
import { DraftId, composerTargetKey, useComposerDraftStore } from "../../composerDraftStore";
import { formatInlineContextReference } from "../../lib/composerContextReferences";
import { rememberCopiedComposerAttachments } from "../../lib/copiedComposerAttachments";
import { countMovedQuestionImports, questionAttachmentDraftId } from "../../questionAttachments";
import { toastManager } from "../ui/toast";
import { pendingDraftWork } from "./pendingDraftWork";
import {
  addPreparingComposerImages,
  removePreparingComposerImages,
} from "./preparingComposerImages";
import { useComposerContextImport } from "./useComposerContextImport";

vi.mock("~/state/use-atom-query-runner", async (importOriginal) => ({
  ...(await importOriginal<typeof import("~/state/use-atom-query-runner")>()),
  useAtomQueryRunner: () => async () => ({ _tag: "Success", value: { relativeUrl: "/file" } }),
}));
vi.mock("../../state/use-atom-command", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../state/use-atom-command")>()),
  useAtomCommand: () => () => {},
}));
vi.mock("~/state/session", async (importOriginal) => ({
  ...(await importOriginal<typeof import("~/state/session")>()),
  readPreparedConnection: () => ({ httpBaseUrl: "http://localhost" }),
}));

type ImportInput = Parameters<typeof useComposerContextImport>[0];
type Importer = ReturnType<typeof useComposerContextImport>;

function Harness({ input, ready }: { input: ImportInput; ready: (value: Importer) => void }) {
  const importer = useComposerContextImport(input);
  useEffect(() => ready(importer));
  return null;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((fulfill) => {
    resolve = fulfill;
  });
  return { promise, resolve };
}

it.each([
  { kind: "file", edit: "delete" },
  { kind: "image", edit: "delete" },
  { kind: "image", edit: "retain" },
  { kind: "file", edit: "undo" },
  { kind: "file", edit: "retain" },
  { kind: "file", edit: "switch" },
  { kind: "file", edit: "cached" },
  { kind: "file", edit: "question" },
  { kind: "image", edit: "question" },
  { kind: "file", edit: "stopped-question" },
  { kind: "image", edit: "stopped-question" },
  { kind: "image", edit: "stopped-question-last-slot" },
  { kind: "image", edit: "stopped-question-full" },
  { kind: "image", edit: "answered-question" },
  { kind: "image", edit: "annotation" },
  { kind: "image", edit: "delete-annotation" },
] as const)("settles a delayed $kind import after $edit", async ({ kind, edit }) => {
  // A stopped question returns its unsent answer, and the import, to the thread draft. One
  // answered on another device drops it, even with this composer unmounted and its ref stale.
  const stopped = edit.startsWith("stopped-question");
  const answered = edit === "answered-question";
  const question = edit === "question" || stopped || answered;
  const environmentId = EnvironmentId.make("clipboard-target");
  const thread = scopeThreadRef(environmentId, ThreadId.make(`clipboard-${kind}-${edit}`));
  const requestId = RuntimeRequestId.make("question");
  const target =
    stopped || answered
      ? questionAttachmentDraftId(environmentId, thread.threadId, requestId, "answer")
      : DraftId.make(`clipboard-${kind}-${edit}`);
  const store = useComposerDraftStore.getState();
  using _draft = {
    [Symbol.dispose]: () => {
      store.clearComposerContent(target);
      store.clearComposerContent(thread);
    },
  };
  const sourceEnvironmentId = EnvironmentId.make("clipboard-source");
  rememberCopiedComposerAttachments(
    sourceEnvironmentId,
    edit === "cached"
      ? new Map([["source-attachment", new File(["notes"], "notes.txt")]])
      : new Map(),
  );
  using _copy = {
    [Symbol.dispose]: () => rememberCopiedComposerAttachments(sourceEnvironmentId, new Map()),
  };
  const promptRef = { current: "" };
  const downloaded = deferred<Response>();
  const requested = deferred<void>();
  const settled = deferred<void>();
  const input = {
    environmentId,
    linkedEnvironments: new Map(),
    activeThread: undefined,
    activeThreadId: null,
    composerDraftTarget: target,
    attachmentDraftTarget: target,
    attachmentTargetKey: target,
    attachmentTargetKeyRef: { current: target },
    questionAttachmentTarget: question ? target : null,
    pendingUserInputs: [],
    composerContextRecords: new Map(),
    composerImages: [],
    composerFilesRef: { current: [] },
    promptRef,
    addComposerFilesToDraft: (files: Parameters<typeof store.addFiles>[1]) =>
      store.addFiles(target, files, { appendReference: !question }),
  } satisfies ImportInput & { promptRef: { current: string } };
  let importer!: Importer;
  await using _screen = await render(
    <Harness
      input={input}
      ready={(value) => {
        importer = value;
      }}
    />,
  );
  const end = pendingDraftWork.end.bind(pendingDraftWork);
  const settledKey = stopped ? composerTargetKey(thread) : target;
  using _settled = vi.spyOn(pendingDraftWork, "end").mockImplementation((key) => {
    end(key);
    if (key === settledKey) settled.resolve();
  });
  using _fetch = stubGlobal("fetch", () => {
    requested.resolve();
    return downloaded.promise;
  });
  const contextId = ComposerContextId.make(`${kind}_source`);
  const attachmentFields = {
    version: 1 as const,
    contextId,
    label: "notes.txt",
    attachmentId: "source-attachment",
    name: "notes.txt",
    mimeType: kind === "file" ? "text/plain" : "image/png",
    sizeBytes: 5,
  };
  const attachment = (
    kind === "file"
      ? { ...attachmentFields, kind: "file" as const }
      : {
          ...attachmentFields,
          kind: "image" as const,
          source: {
            kind: "snap-shot" as const,
            appName: "Finder",
            windowTitle: "Downloads",
            capturedAt: "2026-01-01T00:00:00.000Z",
            accessibleText: "Downloads",
          },
        }
  ) satisfies ComposerContextRecord;
  const annotation = {
    version: 1,
    kind: "preview-annotation",
    contextId: ComposerContextId.make("preview-annotation_source"),
    label: "Page annotation",
    annotationId: "source",
    pageUrl: "http://localhost/page",
    pageTitle: "Page",
    comment: "Check this",
    targetSummary: "Page",
    styleChanges: [],
    screenshotContextId: contextId,
  } satisfies ComposerContextRecord;
  const annotated = edit === "annotation" || edit === "delete-annotation";
  let pastedPrompt = "";
  await act(async () => {
    const remapped = importer.importContextRecords(
      annotated ? [attachment, annotation] : [attachment],
      sourceEnvironmentId,
    );
    pastedPrompt = formatInlineContextReference({
      kind: annotated ? "preview-annotation" : kind,
      contextId: remapped.get(annotated ? annotation.contextId : contextId)!,
      label: annotated ? "Page annotation" : "notes.txt",
    });
    if (question) pastedPrompt = "Question answer";
    promptRef.current = pastedPrompt;
    store.setPrompt(target, pastedPrompt);
    if (edit !== "cached") await requested.promise;
  });
  if (edit !== "cached") expect(pendingDraftWork.has(target)).toBe(true);
  await act(() => {
    if (
      edit !== "annotation" &&
      edit !== "retain" &&
      edit !== "cached" &&
      !question &&
      edit !== "switch"
    ) {
      promptRef.current = "Keep this edit";
      store.setPrompt(target, promptRef.current);
    }
    if (edit === "undo") {
      promptRef.current = pastedPrompt;
      store.setPrompt(target, pastedPrompt);
    }
    if (edit === "switch") input.attachmentTargetKeyRef.current = DraftId.make("other-draft");
    if (stopped || answered) {
      makeThreadRequestsResolvedAnnouncer(
        environmentId,
        thread.threadId,
      )([{ id: requestId, status: stopped ? "cancelled" : "resolved" }]);
      promptRef.current = "";
    }
    if (stopped) input.attachmentTargetKeyRef.current = DraftId.make(composerTargetKey(thread));
  });
  // The thread's send is blocked by, and its slots count, the import that moved there.
  if (stopped) {
    expect(pendingDraftWork.has(composerTargetKey(thread))).toBe(true);
    expect(countMovedQuestionImports(composerTargetKey(thread))).toBe(1);
  }
  // Filling the thread's other slots, the import lands only while its own slot is free.
  if (edit === "stopped-question-last-slot" || edit === "stopped-question-full") {
    store.addFiles(
      thread,
      Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 1 }, (_, index) => ({
        type: "file" as const,
        id: `held-${index}`,
        name: `held-${index}.txt`,
        mimeType: "text/plain",
        sizeBytes: 1,
        file: new File(["x"], `held-${index}.txt`),
      })),
      { appendReference: false },
    );
  }
  if (edit === "stopped-question-full") {
    addPreparingComposerImages(composerTargetKey(thread), [
      { id: "reserved", name: "shot.png", file: new File(["png"], "shot.png"), hidden: false },
    ]);
  }
  using _preparing = {
    [Symbol.dispose]: () =>
      removePreparingComposerImages(composerTargetKey(thread), new Set(["reserved"])),
  };
  await act(async () => {
    downloaded.resolve(new Response("notes", { headers: { "Content-Type": attachment.mimeType } }));
    await settled.promise;
  });
  expect(pendingDraftWork.has(target)).toBe(false);
  expect(pendingDraftWork.has(composerTargetKey(thread))).toBe(false);
  expect(countMovedQuestionImports(composerTargetKey(thread))).toBe(0);
  const draft = store.getComposerDraft(stopped ? thread : target);
  expect(draft?.prompt ?? "").toBe(promptRef.current);
  const attachments = (kind === "file" ? draft?.files : draft?.images) ?? [];
  if (kind === "image" && edit === "retain")
    expect(draft?.images[0]?.source).toEqual(
      attachment.kind === "image" ? attachment.source : undefined,
    );
  expect(attachments.map((file) => file.name)).toEqual(
    edit === "undo" ||
      edit === "retain" ||
      edit === "annotation" ||
      edit === "cached" ||
      (question && !answered && edit !== "stopped-question-full")
      ? ["notes.txt"]
      : [],
  );
});

it.each(["same", "other-environment"] as const)(
  "preserves thread identity across repeated pastes: %s",
  async (variant) => {
    const otherEnvironment = variant === "other-environment";
    const target = DraftId.make(`thread-collision-${variant}`);
    const store = useComposerDraftStore.getState();
    using _draft = { [Symbol.dispose]: () => store.clearComposerContent(target) };
    const environmentId = EnvironmentId.make("thread-source");
    const otherEnvironmentId = EnvironmentId.make("other-thread-source");
    const first = threadContextRecord(
      { environmentId, threadId: ThreadId.make("same-id") },
      "First",
    );
    const pasted = otherEnvironment
      ? threadContextRecord(
          { environmentId: otherEnvironmentId, threadId: first.threadId },
          "Second",
        )
      : first;
    store.addThreadContexts(target, [first], { appendReference: false });
    let importer!: Importer;
    await using _screen = await render(
      <Harness
        input={{
          environmentId,
          linkedEnvironments: new Map([[otherEnvironmentId, "Other machine"]]),
          activeThread: undefined,
          activeThreadId: null,
          composerDraftTarget: target,
          attachmentDraftTarget: target,
          attachmentTargetKey: target,
          attachmentTargetKeyRef: { current: target },
          questionAttachmentTarget: null,
          pendingUserInputs: [],
          composerContextRecords: new Map([[first.contextId, { kind: "thread", record: first }]]),
          composerImages: [],
          composerFilesRef: { current: [] },
          promptRef: { current: "" },
          addComposerFilesToDraft: () => [],
        }}
        ready={(value) => {
          importer = value;
        }}
      />,
    );
    let importedId: string | undefined;
    await act(() => {
      importedId = importer
        .importContextRecords([pasted], pasted.environmentId, target)
        .get(pasted.contextId);
    });
    const records = store.getComposerDraft(target)!.threadContexts;
    expect(records.find((record) => record.contextId === importedId)).toMatchObject({
      environmentId: pasted.environmentId,
      threadId: pasted.threadId,
    });
    expect(records).toHaveLength(otherEnvironment ? 2 : 1);
    expect(records[0]).toEqual(first);
    await act(() => {
      const repeated = importer.importContextRecords([pasted], pasted.environmentId, target);
      expect(repeated.get(pasted.contextId)).toBe(importedId);
    });
    expect(store.getComposerDraft(target)!.threadContexts).toEqual(records);
  },
);

it.each([
  { held: "a pending attachment's reserved slot", marker: false },
  { held: "a reattach marker for it", marker: true },
])("settles a cached paste into a full draft holding $held", async ({ marker }) => {
  const target = DraftId.make(`cached-paste-${marker ? "marker" : "reserved"}`);
  const store = useComposerDraftStore.getState();
  using _draft = { [Symbol.dispose]: () => store.clearComposerContent(target) };
  const reattach = {
    type: "file" as const,
    id: "marker",
    name: "notes.txt",
    mimeType: "text/plain",
    sizeBytes: 5,
    file: null,
  };
  if (marker) store.addFiles(target, [reattach]);
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
  if (!marker) {
    addPreparingComposerImages(target, [
      { id: "reserved", name: "shot.png", file: new File(["png"], "shot.png"), hidden: false },
    ]);
  }
  using _preparing = {
    [Symbol.dispose]: () => removePreparingComposerImages(target, new Set(["reserved"])),
  };
  const sourceEnvironmentId = EnvironmentId.make("cached-paste-source");
  rememberCopiedComposerAttachments(
    sourceEnvironmentId,
    new Map([["source-attachment", new File(["notes"], "notes.txt", { type: "text/plain" })]]),
  );
  using _copy = {
    [Symbol.dispose]: () => rememberCopiedComposerAttachments(sourceEnvironmentId, new Map()),
  };
  let importer!: Importer;
  await using _screen = await render(
    <Harness
      input={{
        environmentId: EnvironmentId.make("cached-paste-target"),
        linkedEnvironments: new Map(),
        activeThread: undefined,
        activeThreadId: null,
        composerDraftTarget: target,
        attachmentDraftTarget: target,
        attachmentTargetKey: target,
        attachmentTargetKeyRef: { current: target },
        questionAttachmentTarget: null,
        pendingUserInputs: [],
        composerContextRecords: new Map(),
        composerImages: [],
        composerFilesRef: { current: marker ? [reattach] : [] },
        promptRef: { current: "" },
        addComposerFilesToDraft: (files) => store.addFiles(target, files),
      }}
      ready={(value) => {
        importer = value;
      }}
    />,
  );

  using toasts = vi.spyOn(toastManager, "add");
  await act(() => {
    importer.importContextRecords(
      [
        {
          version: 1,
          kind: "file",
          contextId: ComposerContextId.make("file_source"),
          label: "notes.txt",
          attachmentId: "source-attachment",
          name: "notes.txt",
          mimeType: "text/plain",
          sizeBytes: 5,
        },
      ],
      sourceEnvironmentId,
    );
  });

  await vi.waitFor(() => expect(pendingDraftWork.has(target)).toBe(false));
  const notes = store.getComposerDraft(target)?.files.find(({ name }) => name === "notes.txt");
  if (marker) {
    expect(notes?.file).toBeInstanceOf(File);
    expect(toasts).not.toHaveBeenCalled();
  } else {
    expect(notes).toBeUndefined();
    expect(toasts).toHaveBeenCalledWith(
      expect.objectContaining({
        description: expect.stringContaining("The attachment limit has been reached."),
      }),
    );
  }
});

it("lands two moved question imports that settle together in the thread's last two slots", async () => {
  const environmentId = EnvironmentId.make("moved-pair-target");
  const thread = scopeThreadRef(environmentId, ThreadId.make("moved-pair"));
  const threadKey = composerTargetKey(thread);
  const requestId = RuntimeRequestId.make("moved-pair-question");
  const target = questionAttachmentDraftId(environmentId, thread.threadId, requestId, "answer");
  const store = useComposerDraftStore.getState();
  using _draft = {
    [Symbol.dispose]: () => {
      store.clearComposerContent(target);
      store.clearComposerContent(thread);
    },
  };
  const sourceEnvironmentId = EnvironmentId.make("moved-pair-source");
  const downloaded = deferred<void>();
  const requested = Promise.withResolvers<void>();
  let requests = 0;
  // In-memory bodies resume both downloads in one microtask checkpoint, as a real race can.
  using _fetch = stubGlobal("fetch", () => {
    if (++requests === 2) requested.resolve();
    return downloaded.promise.then(() => ({
      ok: true,
      blob: async () => new Blob(["notes"], { type: "text/plain" }),
    }));
  });
  const settled = Promise.withResolvers<void>();
  let ends = 0;
  const end = pendingDraftWork.end.bind(pendingDraftWork);
  using _settled = vi.spyOn(pendingDraftWork, "end").mockImplementation((key) => {
    end(key);
    if (key === threadKey && ++ends === 2) settled.resolve();
  });
  const input = {
    environmentId,
    linkedEnvironments: new Map(),
    activeThread: undefined,
    activeThreadId: null,
    composerDraftTarget: target,
    attachmentDraftTarget: target,
    attachmentTargetKey: target,
    attachmentTargetKeyRef: { current: target },
    questionAttachmentTarget: target,
    pendingUserInputs: [],
    composerContextRecords: new Map(),
    composerImages: [],
    composerFilesRef: { current: [] },
    promptRef: { current: "" },
    addComposerFilesToDraft: (files: Parameters<typeof store.addFiles>[1]) =>
      store.addFiles(target, files, { appendReference: false }),
  } satisfies ImportInput;
  let importer!: Importer;
  await using _screen = await render(
    <Harness
      input={input}
      ready={(value) => {
        importer = value;
      }}
    />,
  );
  const record = (name: string) => ({
    version: 1 as const,
    kind: "file" as const,
    contextId: ComposerContextId.make(`file_${name.replace(".", "-")}`),
    label: name,
    attachmentId: name,
    name,
    mimeType: "text/plain",
    sizeBytes: 5,
  });

  await act(async () => {
    importer.importContextRecords([record("a.txt"), record("b.txt")], sourceEnvironmentId);
    await requested.promise;
  });
  await act(() => {
    makeThreadRequestsResolvedAnnouncer(
      environmentId,
      thread.threadId,
    )([{ id: requestId, status: "cancelled" }]);
    input.attachmentTargetKeyRef.current = DraftId.make(threadKey);
  });
  expect(countMovedQuestionImports(threadKey)).toBe(2);
  store.addFiles(
    thread,
    Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 2 }, (_, index) => ({
      type: "file" as const,
      id: `held-${index}`,
      name: `held-${index}.txt`,
      mimeType: "text/plain",
      sizeBytes: 1,
      file: new File(["x"], `held-${index}.txt`),
    })),
    { appendReference: false },
  );
  await act(async () => {
    downloaded.resolve();
    await settled.promise;
  });

  const names = store.getComposerDraft(thread)?.files.map(({ name }) => name) ?? [];
  expect(names.slice(-2).toSorted()).toEqual(["a.txt", "b.txt"]);
});

it("pasting back a removed queued attachment counts against the queued message's limit", async () => {
  const environmentId = EnvironmentId.make("queued-restore-target");
  const target = DraftId.make("queued-restore-edit");
  const store = useComposerDraftStore.getState();
  using _draft = { [Symbol.dispose]: () => store.clearComposerContent(target) };
  // The replacement file took the slot the removed attachment left.
  store.addFiles(
    target,
    [
      {
        type: "file",
        id: "replacement",
        name: "replacement.txt",
        mimeType: "text/plain",
        sizeBytes: 1,
        file: new File(["x"], "replacement.txt"),
      },
    ],
    { appendReference: false },
  );
  const removed = {
    type: "file" as const,
    id: ChatAttachmentId.make("queued-notes"),
    name: "notes.txt",
    mimeType: "text/plain",
    sizeBytes: 5,
  };
  const contextId = ComposerContextId.make("file_queued-notes");
  const onRestoreQueuedAttachment = vi.fn();
  let importer!: Importer;
  await using _screen = await render(
    <Harness
      input={{
        environmentId,
        linkedEnvironments: new Map(),
        activeThread: undefined,
        activeThreadId: null,
        composerDraftTarget: target,
        attachmentDraftTarget: target,
        attachmentTargetKey: target,
        attachmentTargetKeyRef: { current: target },
        questionAttachmentTarget: null,
        pendingUserInputs: [],
        composerContextRecords: new Map(),
        composerImages: [],
        composerFilesRef: { current: [] },
        promptRef: { current: "" },
        addComposerFilesToDraft: () => [],
        existingAttachmentCount: PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 1,
        removedQueuedAttachments: [{ attachment: removed, contextIds: [contextId] }],
        onRestoreQueuedAttachment,
      }}
      ready={(value) => {
        importer = value;
      }}
    />,
  );

  using toasts = vi.spyOn(toastManager, "add");
  let remapped: ReadonlyMap<string, string> = new Map();
  await act(() => {
    remapped = importer.importContextRecords(
      [
        {
          version: 1,
          kind: "file",
          contextId,
          label: "notes.txt",
          attachmentId: removed.id,
          name: "notes.txt",
          mimeType: "text/plain",
          sizeBytes: 5,
        },
      ],
      environmentId,
    );
  });

  expect(onRestoreQueuedAttachment).not.toHaveBeenCalled();
  expect(remapped.has(contextId)).toBe(false);
  expect(toasts).toHaveBeenCalledWith(
    expect.objectContaining({
      description: expect.stringContaining("The attachment limit has been reached."),
    }),
  );
});
