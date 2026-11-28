import { createMemoryHistory } from "@tanstack/react-router";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
} from "@onicode/client-runtime/connection";
import { scopeProjectRef, scopeThreadRef } from "@onicode/client-runtime/environment";
import {
  EnvironmentId,
  IssueTrackerKind,
  ORCHESTRATION_V2_WS_METHODS,
  ProviderDriverKind,
  ProviderInstanceId,
  ProviderThreadId,
  ProviderUploadFeedbackError,
  RunId,
  SourceControlProviderKind,
  ThreadId,
  WS_METHODS,
  type IssueDetail,
  type IssueSummary,
  type IssueRef,
  type OrchestrationV2ProviderThread,
  type ProviderUploadFeedbackInput,
  type ThreadIssueLink,
} from "@onicode/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@onicode/contracts/settings";
import type * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Schema from "effect/Schema";
import * as Effect from "effect/Effect";
import { runAtomCommand } from "@onicode/client-runtime/state/runtime";
import * as Stream from "effect/Stream";
import { expect, it, vi } from "vite-plus/test";
import { cdp, page, userEvent } from "vite-plus/test/browser";

import { AppRoot } from "~/AppRoot";
// The route loads ChatView lazily, and its first transform (React Compiler over ~4k lines)
// takes 6-11s; importing it here keeps that out of the test's timeout.
import "~/components/ChatView";
import { composerTargetKey, DraftId, useComposerDraftStore } from "~/composerDraftStore";
import { environmentCatalog } from "~/connection/catalog";
import {
  addPreparingComposerImages,
  cancelPreparingComposerImages,
  removePreparingComposerImages,
} from "~/components/chat/preparingComposerImages";
import { selectThreadDiffPanelSelection, useDiffPanelStore } from "~/diffPanelStore";
import { createNewThreadHandler } from "~/hooks/useHandleNewThread";
import { __setClientSettingsForTests } from "~/hooks/useSettings";
// Without the app styles the composer overlay sits in flow, so its height loops the timeline.
import "~/index.css";
import { environmentContextRecord } from "~/lib/composerContextRecords";
import { startWorkspaceFocus } from "~/lib/workspaceFocus";
import { usePromptStashStore, writePromptStashStorageForTest } from "~/promptStashStore";
import { useRightPanelStore } from "~/rightPanelStore";
import { getRouter } from "~/router";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { getProjectFileQueryAtom } from "~/components/files/projectFilesQueryState";
import { environmentThreadDetails } from "~/state/threads";
import {
  ENVIRONMENT_ID,
  PROJECT_ID,
  answerAssetUrls,
  answerRpc,
  assistantItem,
  dispatchedCommands,
  dropConnection,
  emitProjectRepositoryIdentity,
  emitShellThread,
  emitThreadEvent,
  hasPendingAssetUrl,
  installFakeEnvironment,
  projectionOf,
  rejectPendingCommand,
  threadWithHistory,
  threadWithQuestion,
  type FakeDriver,
} from "../../test/fakeEnvironment";
import { act, fakeTimers, render } from "../../test/render";

const PNG = Uint8Array.from(
  atob(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
  ),
  (char) => char.charCodeAt(0),
);

/**
 * Accepts every attachment upload without sending its bytes, until disposed. Each waits for
 * `gate`, when given, before it lands.
 */
function acceptAttachmentUploads(gate?: () => Promise<void>): Disposable {
  const mint = answerRpc(WS_METHODS.attachmentsCreateUploadUrl, () =>
    Effect.succeed({ attachmentId: "upload-1", relativeUrl: "/upload", expiresAt: 0 }),
  );
  // Sending verifies each ready upload still exists on the server.
  const verify = answerRpc(WS_METHODS.assetsCreateUrl, () =>
    Effect.succeed({ relativeUrl: "/upload-1", expiresAt: 0 }),
  );
  const status = vi.spyOn(XMLHttpRequest.prototype, "status", "get").mockReturnValue(200);
  const send = vi.spyOn(XMLHttpRequest.prototype, "send").mockImplementation(function (
    this: XMLHttpRequest,
  ) {
    const land = () => this.dispatchEvent(new ProgressEvent("load"));
    if (gate) void gate().then(land);
    else queueMicrotask(land);
  });
  return {
    [Symbol.dispose]: () => {
      send.mockRestore();
      status.mockRestore();
      mint[Symbol.dispose]();
      verify[Symbol.dispose]();
    },
  };
}

// One input event, where typing a key at a time takes seconds on a loaded CI runner.
const insertText = (text: string) => cdp().send("Input.insertText", { text });

class LaunchRefused extends Schema.TaggedError<LaunchRefused>()("LaunchRefused", {}) {}

/** Holds every call to `method` until `release`, then ends it with `end`, until disposed. */
function holdRpc(method: string, end: Effect.Effect<unknown, Error>) {
  let release = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let markCalled = () => {};
  const called = new Promise<void>((resolve) => {
    markCalled = resolve;
  });
  const answer = answerRpc(method, () => {
    markCalled();
    return Effect.promise(() => held).pipe(Effect.andThen(end));
  });
  return { called, release, [Symbol.dispose]: () => answer[Symbol.dispose]() };
}

// The socket is the only fake layer.
vi.mock("../../../../packages/client-runtime/src/connection/driver.ts", async (importOriginal) => {
  const [actual, { fakeDriver }, Layer] = await Promise.all([
    importOriginal<{ readonly ConnectionDriver: Context.Key<unknown, FakeDriver> }>(),
    import("../../test/fakeEnvironment"),
    import("effect/Layer"),
  ]);
  return { ...actual, layer: Layer.succeed(actual.ConnectionDriver, fakeDriver) };
});

vi.mock(import("~/environments/primary/target"), async (original) => {
  const [actual, { fakePrimaryEnvironmentTarget }] = await Promise.all([
    original(),
    import("../../test/fakeEnvironment"),
  ]);
  return {
    ...actual,
    readPrimaryEnvironmentTarget: fakePrimaryEnvironmentTarget,
    resolvePrimaryEnvironmentHttpUrl: (pathname: string) =>
      new URL(pathname, fakePrimaryEnvironmentTarget().httpBaseUrl).toString(),
  };
});

// Null runs the real compression; a test sets it to hold an image in preparation.
const compression = vi.hoisted(() => ({ gate: null as Promise<void> | null }));
vi.mock(import("~/lib/imageCompression"), async (original) => {
  const actual = await original();
  return {
    ...actual,
    prepareImageForAttachment: async (...args) => {
      await compression.gate;
      return actual.prepareImageForAttachment(...args);
    },
  };
});

vi.mock(import("~/rpc/requestLatencyState"), async (original) => ({
  ...(await original()),
  trackRpcRequestSent: () => {},
}));

// A server from before `issues.summary` serves rows the detail, which carries the same fields.
it.each([{ issueSummaries: false }, { issueSummaries: true }])(
  "opens a linked Linear issue from an inactive thread's sidebar badge (summaries: $issueSummaries)",
  async ({ issueSummaries }) => {
    // Each variant reads its own workspace: the app's issue cache outlives a test, and the other
    // variant leaves this issue shipped.
    const workspace = issueSummaries ? "example" : "example-details";
    const first = threadWithHistory("issue-first", 1);
    const linked = threadWithHistory("issue-linked", 1);
    const issueRef = {
      projectId: PROJECT_ID,
      tracker: IssueTrackerKind.make("linear"),
      host: "linear.app",
      scope: "WEB",
      key: "WEB-123",
    } satisfies IssueRef;
    const issue: ThreadIssueLink = {
      ...issueRef,
      url: `https://linear.app/${workspace}/issue/WEB-123/add-keyboard-support`,
      source: "manual",
      linkedAt: "2026-01-01T00:00:00.000Z",
      displayKey: "#123",
      title: "Add keyboard support",
    };
    const completed = {
      ...issue,
      key: "WEB-124",
      displayKey: "WEB-124",
      url: `https://linear.app/${workspace}/issue/WEB-124/keep-tabs-visible`,
      title: "Keep tabs visible",
    };
    const grouped = threadWithHistory("issue-grouped", 1);
    using _environment = installFakeEnvironment(
      [
        first,
        {
          ...linked,
          issues: [issue],
          pullRequests: [
            {
              host: "github.com",
              repository: "aislopware/onicode",
              number: 124,
              url: "https://github.com/aislopware/onicode/pull/124",
              source: "manual",
              linkedAt: "2026-01-01T00:00:00.000Z",
              snapshot: null,
              stack: null,
            },
          ],
        },
        { ...grouped, issues: [completed, issue] },
      ],
      { issueSummaries },
    );
    const liveTitle = "Add keyboard shortcuts to the sidebar";
    const liveDetail = {
      ...issue,
      title: liveTitle,
      author: { login: "sam", name: null, avatarUrl: null },
      state: { category: "open", name: "Open" },
      createdAt: issue.linkedAt,
      updatedAt: issue.linkedAt,
      closedAt: null,
      assignees: [],
      labels: [],
      commentCount: 3,
      projectTitle: "render",
      writable: false,
      capabilities: {
        create: false,
        edit: false,
        comment: false,
        labels: false,
        assignees: false,
        reactions: false,
        templates: false,
        linkedPullRequests: true,
        events: false,
        search: false,
      },
      viewerPermissions: {
        setState: false,
        comment: false,
        edit: false,
        labels: false,
        assignees: false,
      },
      states: [],
      viewer: null,
      body: Array.from({ length: 40 }, (_, index) => `Reproduction step ${index + 1}`).join("\n\n"),
      comments: [],
      commentsCursor: null,
      events: [],
      linkedPullRequests: [
        {
          repository: "aislopware/onicode",
          number: 124,
          url: "https://github.com/aislopware/onicode/pull/124",
          title: "Fix keyboard shortcuts",
          state: "open",
          isDraft: false,
          closesIssue: false,
        },
      ],
    } satisfies IssueDetail;
    const completedDetail = {
      ...liveDetail,
      key: completed.key,
      displayKey: completed.displayKey,
      url: completed.url,
      title: completed.title,
      state: { category: "closed", closeReason: "completed", name: "Done" },
      capabilities: { ...liveDetail.capabilities, linkedPullRequests: false },
      body: "",
      linkedPullRequests: [],
    } satisfies IssueDetail;
    const detailFor = (input: IssueRef) => {
      const isCompleted = input.key === completed.key;
      expect(input).toEqual({ ...readRef, key: isCompleted ? completed.key : issue.key });
      return isCompleted ? completedDetail : liveDetail;
    };
    // Rows read the light summary; the panel the row opens reads the detail. Both keep the
    // workspace the linked URL names, so another workspace's WEB-123 is never read in its place.
    const readRef = { ...issueRef, workspace };
    using _summary = answerRpc(WS_METHODS.issuesSummary, (input: IssueRef) => {
      expect(issueSummaries).toBe(true);
      const read = detailFor(input);
      const summary: IssueSummary = {
        tracker: read.tracker,
        host: read.host,
        scope: read.scope,
        key: read.key,
        displayKey: read.displayKey,
        title: read.title,
        url: read.url,
        author: read.author,
        state: read.state,
        createdAt: read.createdAt,
        updatedAt: read.updatedAt,
        closedAt: read.closedAt,
        assignees: read.assignees,
        labels: read.labels,
        commentCount: read.commentCount,
      };
      return Effect.succeed(summary);
    });
    let panelOpened = false;
    const detailKeys: Array<string> = [];
    using _detail = answerRpc(WS_METHODS.issuesDetail, (input: IssueRef) => {
      detailKeys.push(input.key);
      // With summaries, only the issue panel reads the full detail.
      if (issueSummaries) expect(panelOpened).toBe(true);
      return Effect.succeed(detailFor(input));
    });
    __setClientSettingsForTests({
      ...DEFAULT_CLIENT_SETTINGS,
      onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
    });
    await page.viewport(1400, 900);
    const router = getRouter(
      createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${first.id}`] }),
    );
    await router.load();
    await using _app = await render(<AppRoot router={router} />, {
      // AppRoot brings its own router.
      router: false,
    });
    for (const thread of [first, linked, grouped]) await emitShellThread(thread.id);
    // The badge names the live title the tooltip shows, not the one stored when it was linked.
    const badge = page.getByRole("button", {
      name: `Issue ${issue.displayKey}: ${liveTitle}, Open`,
    });
    await expect.element(badge).toBeVisible();
    expect(badge.element().querySelector("span")).toHaveClass(/text-emerald-600/);
    expect(badge.element()).toHaveTextContent("123");
    expect(badge.element()).not.toHaveTextContent("#");
    const prBadge = page.getByRole("link", { name: "PR #124, status pending" });
    await expect.element(prBadge).toHaveTextContent("124");
    for (const control of [badge, prBadge]) {
      expect(control.element().querySelector("span")).toHaveClass(
        "contents",
        "text-xs",
        "tabular-nums",
      );
    }
    const groupedBadge = page.getByRole("button", { name: "2 linked issues" });
    await expect.element(groupedBadge).toBeVisible();
    if (issueSummaries) {
      // A group shows its open issue's state over a completed one.
      await vi.waitFor(() =>
        expect(groupedBadge.element().querySelector("span")).toHaveClass(/text-emerald-600/),
      );
    } else {
      // An older server has no summaries, and a group never reads full details for an icon.
      expect(detailKeys).not.toContain(completed.key);
    }
    await userEvent.hover(badge);
    const issueName = `Issue ${issue.displayKey}: ${liveTitle}`;
    const previewLink = page.getByRole("link", { name: issueName });
    await userEvent.hover(previewLink.getByText("sam", { exact: true }));
    await expect.element(previewLink).toBeVisible();
    const contextMenu = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    await act(() => previewLink.element().dispatchEvent(contextMenu));
    expect(contextMenu.defaultPrevented).toBe(false);
    await userEvent.click(badge);
    const issueRow = page
      .getByRole("region", { name: "Linked items" })
      .getByRole("link", { name: issueName });
    await expect.element(issueRow.getByRole("img", { name: "Open" })).toBeVisible();
    await expect.element(issueRow.getByRole("img", { name: "3 comments" })).toBeVisible();
    await expect.element(issueRow.getByRole("img", { name: "Linear" })).toBeVisible();
    await expect.element(issueRow.getByText(issue.scope, { exact: true })).toBeVisible();
    panelOpened = true;
    await userEvent.click(issueRow);
    expect(router.history.location.pathname).toBe(`/${ENVIRONMENT_ID}/${linked.id}`);
    await expect.element(page.getByRole("heading", { name: liveTitle })).toBeVisible();
    const linkedPr = page.getByRole("link", { name: /Fix keyboard shortcuts/ });
    const issueScroller = () =>
      linkedPr.element().closest<HTMLElement>("[data-pull-request-summary-scroll]")!;
    const prRow = await linkedPr.findElement();
    await expect.element(page.getByText("Reproduction step 40", { exact: true })).toBeVisible();
    // The panel saves its position from the scroll event, which lands after scrollIntoView.
    const scrolled = new Promise((resolve) =>
      issueScroller().addEventListener("scroll", resolve, { once: true }),
    );
    prRow.scrollIntoView({ block: "center" });
    await scrolled;
    const issueScrollTop = issueScroller().scrollTop;
    expect(issueScrollTop).toBeGreaterThan(0);
    // A Linear issue's pull request opens through its repository's project, which this fixture
    // lacks, so the pull request tab opens from the store.
    await act(() =>
      useRightPanelStore.getState().openPullRequest(scopeThreadRef(ENVIRONMENT_ID, linked.id), {
        projectId: PROJECT_ID,
        host: "github.com",
        repository: "aislopware/onicode",
        number: 124,
        url: "https://github.com/aislopware/onicode/pull/124",
      }),
    );
    await expect.element(linkedPr).not.toBeInTheDocument();
    await userEvent.click(page.getByRole("button", { name: issue.key, exact: true }));
    await vi.waitFor(() => expect(issueScroller().scrollTop).toBe(issueScrollTop));
    // Closed elsewhere: refreshing the panel also refreshes the state its tab shows.
    Object.assign(liveDetail, { state: { category: "closed", name: "Shipped" } });
    await userEvent.click(page.getByRole("button", { name: "Refresh issue" }));
    await vi.waitFor(() =>
      expect(
        document.querySelector("[data-active-tab='true'] [role='img'][aria-label='Shipped']"),
      ).toBeVisible(),
    );
    await act(() => useRightPanelStore.getState().close(scopeThreadRef(ENVIRONMENT_ID, linked.id)));
    await expect
      .element(
        page.getByRole("region", { name: "Thread details" }).getByRole("link", {
          name: issueName,
          exact: true,
        }),
      )
      .toBeVisible();
  },
);

it("reads only the linked issues a thread shows", async () => {
  const many = threadWithHistory("issue-many", 1);
  const few = threadWithHistory("issue-few", 1);
  const issueLinks = (count: number) =>
    Array.from({ length: count }, (_, index): ThreadIssueLink => {
      const key = String(index + 1);
      return {
        projectId: PROJECT_ID,
        tracker: IssueTrackerKind.make("github"),
        host: "github.com",
        scope: "owner/repo",
        key,
        url: `https://github.com/owner/repo/issues/${key}`,
        source: "agent",
        linkedAt: "2026-01-01T00:00:00.000Z",
        displayKey: `#${key}`,
        title: `Issue number ${key}`,
      };
    });
  const issues = issueLinks(100);
  using _environment = installFakeEnvironment([
    { ...many, issues },
    { ...few, issues: issueLinks(5) },
  ]);
  await emitShellThread(many.id);
  await emitShellThread(few.id);
  const read = new Set<string>();
  // Left pending: a row reading its summary is the cost under test.
  using _summary = answerRpc(WS_METHODS.issuesSummary, (input: IssueRef) => {
    read.add(input.key);
    return Effect.never;
  });
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  await page.viewport(1400, 900);
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${many.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const details = page.getByRole("region", { name: "Thread details" });
  const detailsIssues = details.getByRole("link", { name: /^Issue #/ });
  await expect
    .element(details.getByRole("link", { name: "Issue #1: Issue number 1" }))
    .toBeVisible();
  await expect.element(details.getByRole("button", { name: "Show 97 more" })).toBeVisible();
  expect(detailsIssues.elements()).toHaveLength(3);

  await act(() =>
    useRightPanelStore.getState().open(scopeThreadRef(ENVIRONMENT_ID, many.id), "pull-requests"),
  );
  const linkedItems = page.getByRole("region", { name: "Linked items" });
  const linkedIssues = linkedItems.getByRole("link", { name: /^Issue #/ });
  await vi.waitFor(() => expect(linkedIssues.elements().length).toBeGreaterThan(3));
  expect(linkedIssues.elements().length).toBeLessThan(issues.length);
  expect(read.size).toBeLessThan(issues.length);
  const scroller = [...linkedItems.element().querySelectorAll("div")].find(
    (element) => element.scrollHeight > element.clientHeight,
  )!;
  await act(() => scroller.scrollTo({ top: scroller.scrollHeight }));
  await expect
    .element(linkedItems.getByRole("link", { name: "Issue #100: Issue number 100" }))
    .toBeVisible();
  await act(() => useRightPanelStore.getState().close(scopeThreadRef(ENVIRONMENT_ID, many.id)));

  // Expanding one thread's issues leaves the next thread's collapsed.
  await userEvent.click(details.getByRole("button", { name: "Show 97 more" }));
  await vi.waitFor(() => expect(detailsIssues.elements()).toHaveLength(100));
  // Revealed rows read as they scroll into view, not all at once; the panel skipped the middle.
  await act(() =>
    details
      .getByRole("link", { name: "Issue #40: Issue number 40" })
      .element()
      .scrollIntoView({ block: "start" }),
  );
  await vi.waitFor(() => expect(read.has("40")).toBe(true));
  expect(read.has("70")).toBe(false);
  await act(() => router.navigate({ to: `/${ENVIRONMENT_ID}/${few.id}` }));
  await expect.element(details.getByRole("button", { name: "Show 2 more" })).toBeVisible();
  expect(detailsIssues.elements()).toHaveLength(3);

  // Expanding the sidebar badge's preview stays on the open thread.
  // The preview holds the first 8; the rest live in the Linked items tab.
  await userEvent.hover(page.getByRole("button", { name: "100 linked issues" }));
  await userEvent.click(page.getByRole("button", { name: "Show 5 more" }));
  await expect.element(page.getByRole("link", { name: "Issue #8: Issue number 8" })).toBeVisible();
  expect(router.history.location.pathname).toBe(`/${ENVIRONMENT_ID}/${few.id}`);
  await userEvent.click(page.getByRole("button", { name: "+92 more in Linked items" }));
  await expect
    .element(linkedItems.getByRole("link", { name: "Issue #1: Issue number 1" }))
    .toBeVisible();
  expect(router.history.location.pathname).toBe(`/${ENVIRONMENT_ID}/${many.id}`);
});

it("a machine chip stashed in one thread restores with its machine in another", async () => {
  await page.viewport(1400, 900);
  writePromptStashStorageForTest("[]");
  const other = threadWithHistory("thread-stash-restore", 1);
  const asking = threadWithQuestion("thread-stash-question", "Which file should change?");
  using _environment = installFakeEnvironment([other, asking]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftId = DraftId.make("draft-stash-machine");
  const machine = environmentContextRecord(EnvironmentId.make("environment-build"), "Build box");
  const drafts = useComposerDraftStore.getState();
  drafts.setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
    threadId: ThreadId.make("thread-stash-machine"),
  });
  drafts.setPrompt(draftId, "Deploy on ");
  drafts.addEnvironmentContexts(draftId, [machine]);
  const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const editor = page.getByTestId("composer-editor");
  const pressStashShortcut = async () => {
    await userEvent.click(editor);
    await userEvent.keyboard("{ControlOrMeta>}s{/ControlOrMeta}");
  };
  await vi.waitFor(() => expect(editor.element().textContent).toContain("Deploy on"));
  // A draft can't stash until the shell snapshot lists its project.
  await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();
  await pressStashShortcut();
  await vi.waitFor(() => expect(usePromptStashStore.getState().entries).toHaveLength(1));

  // An answer carries no machines, so an open question refuses the restore and keeps the entry.
  await router.navigate({ to: `/${ENVIRONMENT_ID}/${asking.id}` });
  await expect.element(page.getByText("Which file should change?")).toBeVisible();
  await userEvent.click(page.getByRole("button", { name: /^Stashed prompts/ }));
  await userEvent.click(page.getByRole("button", { name: /^Restore stashed prompt/ }));
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("Stashed machines can't go in an answer"),
  );
  expect(usePromptStashStore.getState().entries).toHaveLength(1);

  await router.navigate({ to: `/${ENVIRONMENT_ID}/${other.id}` });
  await expect.element(page.getByText("Answer 0 for thread-stash-restore.")).toBeVisible();
  await pressStashShortcut();

  await vi.waitFor(() => expect(editor.element().textContent).toContain("Deploy on"));
  expect(
    useComposerDraftStore.getState().getComposerDraft(scopeThreadRef(ENVIRONMENT_ID, other.id))
      ?.environmentContexts,
  ).toEqual([machine]);
  expect(
    useComposerDraftStore.getState().getComposerDraft(draftId)?.environmentContexts ?? [],
  ).toEqual([]);
});

it("a question that opens while a stashed file is checked keeps a machine stash", async () => {
  await page.viewport(1400, 900);
  writePromptStashStorageForTest("[]");
  const thread = threadWithHistory("thread-stash-late-question", 1);
  const asking = threadWithQuestion("thread-stash-late-question", "Which file should change?");
  const [request] = asking.runtimeRequests ?? [];
  const question = asking.items.at(-1);
  if (!request || !question) throw new Error("threadWithQuestion has no question");
  using _environment = installFakeEnvironment([thread]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  usePromptStashStore.getState().stashEntry({
    id: "stash-late-question",
    createdAt: "2026-01-01T00:00:00.000Z",
    prompt: "Deploy on ",
    attachments: [],
    files: [
      {
        id: "file-notes",
        name: "notes.txt",
        mimeType: "text/plain",
        sizeBytes: 5,
        attachmentId: "attachment-notes",
        environmentId: ENVIRONMENT_ID,
      },
    ],
    droppedImageNames: [],
    records: [environmentContextRecord(EnvironmentId.make("environment-build"), "Build box")],
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText("Answer 0 for thread-stash-late-question.")).toBeVisible();
  await userEvent.click(page.getByRole("button", { name: /^Stashed prompts/ }));
  await userEvent.click(page.getByRole("button", { name: /^Restore stashed prompt/ }));

  // The question opens while the restore still waits on the server's file check.
  await vi.waitFor(() => expect(hasPendingAssetUrl()).toBe(true));
  await emitThreadEvent(thread.id, { type: "turn-item.updated", payload: question });
  await emitThreadEvent(thread.id, { type: "runtime-request.updated", payload: request });
  await expect.element(page.getByText("Which file should change?")).toBeVisible();
  await answerAssetUrls();

  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("Stashed machines can't go in an answer"),
  );
  expect(usePromptStashStore.getState().entries).toHaveLength(1);
  usePromptStashStore.getState().takeEntry("stash-late-question");
});

it("a pull request link waits for the repository identity a project learns after startup", async () => {
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([]);
  using _linked = answerRpc(WS_METHODS.pullRequestsLinkedThreads, () =>
    Effect.succeed({ threads: [] }),
  );
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(createMemoryHistory({ initialEntries: ["/new?pr=example/demo/42"] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  // The remote was added after the project, so the first snapshot knows no repository.
  await expect.element(page.getByText("You don't have example/demo")).toBeVisible();
  await using _identity = await emitProjectRepositoryIdentity({
    canonicalKey: "github.com/example/demo",
    locator: {
      source: "git-remote",
      remoteName: "origin",
      remoteUrl: "https://github.com/example/demo.git",
    },
    provider: SourceControlProviderKind.make("github"),
    displayName: "example/demo",
    owner: "example",
    name: "demo",
  });
  await expect.element(page.getByText("Check out example/demo#42?")).toBeVisible();
});

it("a new draft does not load its reserved thread before the server lists it", async () => {
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([]);
  using fetchSpy = vi.spyOn(globalThis, "fetch");
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftId = DraftId.make("draft-27443");
  const threadId = ThreadId.make("thread-27443");
  useComposerDraftStore
    .getState()
    .setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, { threadId });

  const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByTestId("composer-editor")).toBeVisible();
  // The shell snapshot has arrived once the sidebar lists the project, so the connection is up.
  await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();

  const threadRequests = fetchSpy.mock.calls
    .map(([input]) => String(input instanceof Request ? input.url : input))
    .filter((url) => url.includes(`/api/orchestration/threads/${threadId}/`));
  // A 404 here marks the thread deleted, and the view never loads it once the send creates it.
  expect(threadRequests).toEqual([]);
});

it.each([
  // A refusal fails at once.
  ["refused", Effect.fail(new LaunchRefused()), "Message not sent"],
  // An interrupted launch waits to be resent, until the host refuses the reconnect.
  ["interrupted", Effect.interrupt, "Message may not have been sent"],
] as const)(
  "an image-only first send of a new thread that is %s keeps the new draft",
  async (outcome, end, title) => {
    await page.viewport(1400, 900);
    using _environment = installFakeEnvironment([]);
    __setClientSettingsForTests({
      ...DEFAULT_CLIENT_SETTINGS,
      onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
    });
    const draftId = DraftId.make(`draft-image-only-${outcome}`);
    useComposerDraftStore
      .getState()
      .setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
        threadId: ThreadId.make(`thread-image-only-${outcome}`),
      });
    using launch = holdRpc(ORCHESTRATION_V2_WS_METHODS.launchThread, end);
    const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
    await router.load();
    await using _app = await render(<AppRoot router={router} />, {
      // AppRoot brings its own router.
      router: false,
    });
    using _uploads = acceptAttachmentUploads();
    const editor = page.getByTestId("composer-editor");
    await expect.element(editor).toBeVisible();
    // The send needs the project, which arrives with the shell snapshot.
    await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();

    await userEvent.click(editor);
    const clipboardData = new DataTransfer();
    clipboardData.items.add(new File([PNG], "shot.png", { type: "image/png" }));
    editor
      .element()
      .dispatchEvent(
        new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
      );
    // The tile shows the image while it still compresses, and a send meanwhile is refused.
    await expect.element(page.getByRole("button", { name: "Preview shot.png" })).toBeEnabled();
    await userEvent.keyboard("{Enter}");
    await launch.called;
    await userEvent.keyboard("next draft");
    await expect.element(editor).toHaveTextContent("next draft");
    launch.release();
    const dropped =
      outcome === "interrupted"
        ? await dropConnection({
            lost: new ConnectionTransientError({ reason: "transport", detail: "Socket closed." }),
            refusal: new ConnectionBlockedError({ reason: "configuration", detail: "Refused." }),
          })
        : null;
    // Later tests in this file share the connection.
    await using _reconnect = {
      [Symbol.asyncDispose]: async () => {
        if (dropped === null) return;
        await runAtomCommand(appAtomRegistry, environmentCatalog.retryNow, ENVIRONMENT_ID);
        await dropped.reconnected;
      },
    };

    await expect.element(page.getByText(title)).toBeVisible();
    await expect.element(editor).toHaveTextContent("next draft");
    // With no text there is nothing to copy.
    expect(page.getByText("Copy message", { exact: true }).query()).toBeNull();
  },
);

it.each([
  "new",
  "existing",
  "background",
  "background behind Settings",
  "existing background behind Settings",
] as const)(
  "Enter while an image compresses shows a cancellable banner, cancels on edit or Esc, and still sends after leaving a %s thread",
  async (kind) => {
    await page.viewport(1400, 900);
    const thread = threadWithHistory(`upload-wait-${kind}`, 1);
    const other = threadWithHistory(`upload-wait-other-${kind}`, 1);
    const existing = kind.startsWith("existing");
    const background = kind.includes("background");
    const behindSettings = kind.endsWith("behind Settings");
    using _environment = installFakeEnvironment(existing ? [thread, other] : [other], {
      acceptCommands: true,
    });
    __setClientSettingsForTests({
      ...DEFAULT_CLIENT_SETTINGS,
      onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
    });
    const draftId = DraftId.make(`draft-upload-wait-${kind}`);
    if (!existing) {
      useComposerDraftStore
        .getState()
        .setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
          threadId: thread.id,
        });
    }
    using launch = holdRpc(
      ORCHESTRATION_V2_WS_METHODS.launchThread,
      background
        ? Effect.succeed({ threadId: thread.id, projection: projectionOf(thread), resumed: false })
        : Effect.interrupt,
    );
    const router = getRouter(
      createMemoryHistory({
        initialEntries: [existing ? `/${ENVIRONMENT_ID}/${thread.id}` : `/draft/${draftId}`],
      }),
    );
    await router.load();
    await using _app = await render(<AppRoot router={router} />, {
      // AppRoot brings its own router.
      router: false,
    });
    const compressed = Promise.withResolvers<void>();
    compression.gate = compressed.promise;
    using _compression = { [Symbol.dispose]: () => (compression.gate = null) };
    using _uploads = acceptAttachmentUploads();
    const editor = page.getByTestId("composer-editor");
    await expect.element(editor).toBeVisible();
    await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();

    await userEvent.click(editor);
    const clipboardData = new DataTransfer();
    clipboardData.items.add(new File([PNG], "shot.png", { type: "image/png" }));
    editor
      .element()
      .dispatchEvent(
        new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
      );

    // Enter arms the send and shows the banner plus a disabled spinner button.
    await userEvent.keyboard("check image{Enter}");
    await expect.element(page.getByText("Sending once image is ready")).toBeVisible();
    const waitingButton = page.getByRole("button", { name: "Sending when ready", exact: true });
    await expect.element(waitingButton).toBeDisabled();
    await expect.element(waitingButton).toHaveAttribute("aria-busy", "true");

    // Typing disarms the queued send and keeps the edit.
    await userEvent.keyboard(" now");
    await expect.element(page.getByText("Sending once image is ready")).not.toBeInTheDocument();
    await expect.element(editor).toHaveTextContent("check image now");

    // Enter arms again; Esc cancels cleanly.
    await userEvent.keyboard("{Enter}");
    await expect.element(page.getByText("Sending once image is ready")).toBeVisible();
    await userEvent.keyboard("{Escape}");
    await expect.element(page.getByText("Sending once image is ready")).not.toBeInTheDocument();

    // Enter arms again; leaving the thread still sends once the image is ready.
    // Existing threads send and start a new thread with Mod+Alt+Enter.
    await userEvent.keyboard(
      !background
        ? "{Enter}"
        : existing
          ? "{ControlOrMeta>}{Alt>}{Enter}{/Alt}{/ControlOrMeta}"
          : "{ControlOrMeta>}{Enter}{/ControlOrMeta}",
    );
    await expect.element(page.getByText("Sending once image is ready")).toBeVisible();
    const awayPath = behindSettings ? "/settings/general" : `/${ENVIRONMENT_ID}/${other.id}`;
    await router.navigate({ to: awayPath });
    if (!behindSettings) {
      await expect.element(page.getByText(`Answer 0 for upload-wait-other-${kind}.`)).toBeVisible();
    }

    compressed.resolve();
    if (background) {
      if (existing) {
        await vi.waitFor(() =>
          expect(dispatchedCommands()).toContainEqual(
            expect.objectContaining({ type: "message.dispatch", threadId: thread.id }),
          ),
        );
      } else {
        await launch.called;
        launch.release();
      }
      // A background send confirms itself without opening a fresh composer over where the user is.
      await expect.element(page.getByText("Started in background")).toBeVisible();
      expect(router.state.location.pathname).toBe(awayPath);
    } else if (kind === "new") {
      await launch.called;
    } else {
      await vi.waitFor(() =>
        expect(dispatchedCommands()).toContainEqual(
          expect.objectContaining({
            type: "message.dispatch",
            threadId: thread.id,
            attachments: [expect.objectContaining({ type: "image", id: "upload-1" })],
          }),
        ),
      );
    }
    launch.release();
  },
);

it("a file picked after the phone composer collapses still attaches", async () => {
  const thread = threadWithHistory("thread-40993", 1);
  using _environment = installFakeEnvironment([thread]);
  await page.viewport(390, 844);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  using _uploads = acceptAttachmentUploads();
  const editor = page.getByTestId("composer-editor");
  await expect.element(page.getByText(`Answer 0 for ${thread.id}.`)).toBeVisible();
  // The phone composer opens collapsed; tapping its prompt row expands it.
  await userEvent.click(page.getByRole("button", { name: "Expand composer" }));
  await expect.element(page.getByRole("button", { name: "Attach files" })).toBeVisible();
  const input = document.querySelector<HTMLInputElement>(
    'form[data-chat-composer-form] input[type="file"]',
  );
  expect(input).not.toBeNull();
  using openPicker = vi.spyOn(input!, "click").mockImplementation(() => {});
  await userEvent.click(page.getByRole("button", { name: "Attach files" }));
  expect(openPicker).toHaveBeenCalledOnce();

  // Opening the native picker blurs the editor, which collapses the phone composer. A blur in
  // the frames right after expanding is ignored, so blur again until it collapses.
  await vi.waitFor(() => {
    const collapsed = document.querySelector("[data-chat-composer-mobile-collapsed='true']");
    if (collapsed !== null) return;
    const element = editor.element() as HTMLElement;
    element.focus();
    element.blur();
    throw new Error("The phone composer is still expanded.");
  });
  const files = new DataTransfer();
  files.items.add(new File([PNG], "picked.png", { type: "image/png" }));
  input!.files = files.files;
  input!.dispatchEvent(new Event("change", { bubbles: true }));

  await expect.element(page.getByRole("button", { name: "Preview picked.png" })).toBeVisible();
});

it("opens the thread actions menu for an older settled thread the shell leaves out", async () => {
  await page.viewport(1400, 900);
  const thread = { ...threadWithHistory("older-settled", 1), olderSettled: true as const };
  using _environment = installFakeEnvironment([thread]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });

  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText("older-settled: question 0")).toBeVisible();

  await page.getByRole("button", { name: `Thread actions for ${thread.title}` }).click();
  // Without a bound the poll runs to the test deadline and reports a bare timeout.
  await expect
    .element(page.getByRole("menuitem", { name: "Rename thread" }), { timeout: 5_000 })
    .toBeVisible();
});

it("retains each thread's diff selection when returning from another thread or Settings", async () => {
  await page.viewport(1400, 900);
  const a = threadWithHistory("diff-selection-a", 1);
  const b = threadWithHistory("diff-selection-b", 1);
  using _environment = installFakeEnvironment([a, b]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const aRef = scopeThreadRef(ENVIRONMENT_ID, a.id);
  const bRef = scopeThreadRef(ENVIRONMENT_ID, b.id);
  useRightPanelStore.getState().open(aRef, "diff");
  useRightPanelStore.getState().open(bRef, "diff");
  useDiffPanelStore.getState().selectBranchBaseRef(bRef, "origin/release");
  const router = getRouter(createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${a.id}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText(`Answer 0 for ${a.id}.`)).toBeVisible();
  await act(() => useDiffPanelStore.getState().selectGitScope(aRef, "unstaged"));

  await act(() =>
    router.navigate({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENVIRONMENT_ID, threadId: b.id },
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${b.id}.`)).toBeVisible();
  expect(selectThreadDiffPanelSelection(useDiffPanelStore.getState().byThreadKey, bRef)).toEqual({
    kind: "branch",
    baseRef: "origin/release",
  });
  await act(() =>
    router.navigate({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENVIRONMENT_ID, threadId: a.id },
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${a.id}.`)).toBeVisible();
  expect(selectThreadDiffPanelSelection(useDiffPanelStore.getState().byThreadKey, aRef)).toEqual({
    kind: "unstaged",
  });

  await act(() => useDiffPanelStore.getState().selectTurn(aRef, a.runs[0]!.id));
  await act(() => router.navigate({ to: "/settings/general" }));
  await expect
    .element(page.getByRole("heading", { name: "Time format", exact: true }))
    .toBeVisible();
  await act(() =>
    router.navigate({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENVIRONMENT_ID, threadId: a.id },
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${a.id}.`)).toBeVisible();
  expect(
    selectThreadDiffPanelSelection(useDiffPanelStore.getState().byThreadKey, aRef),
  ).toMatchObject({
    kind: "turn",
    turnId: a.runs[0]!.id,
  });
});

it("a stashed prompt ending in a file chip restores with the chip", async () => {
  await page.viewport(1400, 900);
  writePromptStashStorageForTest("[]");
  using _environment = installFakeEnvironment([]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftId = DraftId.make("draft-28338");
  const drafts = useComposerDraftStore.getState();
  drafts.setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
    threadId: ThreadId.make("thread-28338"),
  });
  drafts.setPrompt(draftId, "Check [package.json](package.json) ");
  const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const chip = page.getByRole("button", { name: "Preview package.json" });
  await expect.element(chip).toBeVisible();
  // The stash shortcut is a no-op until the shell snapshot lists the project.
  await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();

  const editor = page.getByTestId("composer-editor");
  await userEvent.click(editor);
  await userEvent.keyboard("{ControlOrMeta>}s{/ControlOrMeta}");
  await expect.element(chip).not.toBeInTheDocument();
  await userEvent.keyboard("{ControlOrMeta>}s{/ControlOrMeta}");

  await vi.waitFor(() => expect(editor.element().textContent).toContain("Check"));
  expect(editor.element().textContent).not.toContain("[package.json](package.json)");
  await expect.element(chip).toBeVisible();
});

it("a rejected send leaves the timeline when a new draft was typed meanwhile", async () => {
  await page.viewport(1400, 900);
  const thread = threadWithHistory("rejected-send", 1);
  using _environment = installFakeEnvironment([thread], {
    acceptCommands: true,
    holdCommand: (command) => command.type === "message.dispatch",
  });
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText(`Answer 0 for ${thread.id}.`)).toBeVisible();

  const editor = page.getByTestId("composer-editor");
  await userEvent.click(editor);
  await userEvent.keyboard("Failed prompt{Enter}");
  await vi.waitFor(() =>
    expect(dispatchedCommands().map((command) => command.type)).toContain("message.dispatch"),
  );
  await expect.element(page.getByText("Failed prompt")).toBeVisible();
  await userEvent.keyboard("Next draft");

  await rejectPendingCommand("Attachment 'sample.png' cannot be sent.");

  await expect.element(page.getByText("Attachment 'sample.png' cannot be sent.")).toBeVisible();
  await expect.element(page.getByText("Failed prompt")).not.toBeInTheDocument();
  expect(editor.element().textContent).toContain("Next draft");
});

it("Enter on an empty prompt resumes an interrupted thread", async () => {
  await page.viewport(1400, 900);
  const history = threadWithHistory("interrupted", 1);
  const interruptedRun = { ...history.runs[0]!, status: "interrupted" as const };
  const thread = { ...history, runs: [interruptedRun] };
  using _environment = installFakeEnvironment([thread], { acceptCommands: true });
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByRole("button", { name: "Resume thread" })).toBeVisible();

  await userEvent.click(page.getByTestId("composer-editor"));
  await userEvent.keyboard("{Enter}");

  await vi.waitFor(() =>
    expect(dispatchedCommands()).toContainEqual(
      expect.objectContaining({ manualContinuationOfRunId: interruptedRun.id }),
    ),
  );
});

it("opening a thread or returning to the window on a touch tablet does not focus the composer", async () => {
  const realMatchMedia = window.matchMedia.bind(window);
  using _touch = vi
    .spyOn(window, "matchMedia")
    .mockImplementation((query) => realMatchMedia(query === "(pointer: coarse)" ? "all" : query));
  const first = threadWithHistory("thread-29469-a", 1);
  const second = threadWithHistory("thread-29469-b", 1);
  using _environment = installFakeEnvironment([first, second]);
  // iPad Pro 11 in landscape: the desktop layout, above max-sm.
  await page.viewport(1194, 834);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${first.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const composer = page.getByTestId("composer-editor");
  await expect.element(page.getByText(`Answer 0 for ${first.id}.`)).toBeVisible();

  // Fake frames hold the next thread entry's focus frame until the test advances it.
  using _timers = fakeTimers();
  await act(() =>
    router.navigate({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENVIRONMENT_ID, threadId: second.id },
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${second.id}.`)).toBeVisible();
  vi.advanceTimersToNextFrame();
  // Focusing the editor raises the on-screen keyboard, which shrinks the page on iPadOS.
  expect(document.activeElement).not.toBe(composer.element());

  // ChatView waits two frames after window focus before refocusing the composer.
  window.dispatchEvent(new FocusEvent("focus"));
  vi.advanceTimersToNextFrame();
  vi.advanceTimersToNextFrame();
  expect(document.activeElement).not.toBe(composer.element());
});

it("a mounted thread view lets go of the thread structures it has replaced", async () => {
  await page.viewport(1400, 900);
  // Items are shared with the fixture, so a reopen's snapshot also carries the earlier updates.
  const history = threadWithHistory("updated", 3);
  const items = [...history.items];
  const updated = { ...history, items };
  const other = threadWithHistory("other", 3);
  using _environment = installFakeEnvironment([updated, other]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(createMemoryHistory({ initialEntries: ["/"] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, { router: false });
  const open = async (threadId: ThreadId, text: string) => {
    await act(() =>
      router.navigate({
        to: "/$environmentId/$threadId",
        params: { environmentId: ENVIRONMENT_ID, threadId },
      }),
    );
    await expect.element(page.getByText(text).first()).toBeVisible();
  };
  const structureAtom = environmentThreadDetails.quietThreadAtom(
    scopeThreadRef(ENVIRONMENT_ID, updated.id),
  );

  // Each round, a new answer arrives while the thread is shown, then the user switches away.
  const shown: Array<WeakRef<object>> = [];
  await open(other.id, "Answer 2 for other.");
  for (let round = 0; round < 6; round++) {
    await open(updated.id, round === 0 ? "Answer 2 for updated." : `Update ${round - 1}.`);
    const update = assistantItem(
      updated.id,
      RunId.make("updated-run-2"),
      `updated-update-${round}`,
      6 + round,
      `Update ${round}.`,
    );
    items.push(update);
    await act(() => emitThreadEvent(updated.id, { type: "turn-item.updated", payload: update }));
    await expect.element(page.getByText(`Update ${round}.`)).toBeVisible();
    shown.push(new WeakRef(appAtomRegistry.get(structureAtom)!.projection));
    await open(other.id, "Answer 2 for other.");
  }
  await cdp().send("HeapProfiler.collectGarbage");

  // The thread keeps its latest structure, and the timeline may remember the one before it.
  const kept = shown.slice(0, -2).flatMap((structure, round) => (structure.deref() ? [round] : []));
  expect(kept).toEqual([]);
});

it("a shortcut conditioned on rightPanelOpen sees the thread's open right panel", async () => {
  await page.viewport(1280, 800);
  const thread = threadWithHistory("thread-panel-shortcut", 1);
  using _environment = installFakeEnvironment([thread]);
  using _platform = vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const composer = page.getByTestId("composer-editor");
  const toggle = page.getByRole("button", { name: "Toggle right panel", exact: true });
  await expect.element(toggle).toHaveAttribute("aria-pressed", "false");
  await toggle.click();
  await page.getByRole("button", { name: /^Files/ }).click();
  await composer.click();
  await userEvent.keyboard("{Meta>}t{/Meta}");
  await expect.element(page.getByRole("menuitem", { name: /Files/ })).toBeVisible();
  await expect.element(toggle).toHaveAttribute("aria-pressed", "true");
});

it("Cmd+W closes the selected right-panel tabs together", async () => {
  await page.viewport(1280, 800);
  const thread = threadWithHistory("thread-close-selected-tabs", 1);
  using _environment = installFakeEnvironment([thread]);
  using _platform = vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const threadRef = scopeThreadRef(ENVIRONMENT_ID, thread.id);
  const panels = useRightPanelStore.getState();
  panels.open(threadRef, "diff");
  panels.openFile(threadRef, "notes.md");
  panels.openFile(threadRef, "routes.ts");
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });

  const tab = (name: string) => page.getByRole("button", { name, exact: true });
  await userEvent.click(tab("notes.md"), { modifiers: ["Shift"] });
  await expect.element(tab("notes.md")).toHaveAttribute("aria-pressed", "true");
  await userEvent.keyboard("{Meta>}w{/Meta}");
  await expect.element(tab("notes.md")).not.toBeInTheDocument();
  await expect.element(tab("routes.ts")).not.toBeInTheDocument();
  await expect.element(tab("Diff")).toHaveAttribute("aria-pressed", "true");

  await userEvent.keyboard("{Meta>}w{/Meta}");
  await expect.element(tab("Diff")).not.toBeInTheDocument();
});

it("selecting a side-chat tab focuses that side chat's composer", async () => {
  const parent = threadWithHistory("focus-parent", 1);
  // The shell loaded by the file's first test stays; a family read serves side chats added later.
  const sideChat = (name: string) => ({
    ...threadWithHistory(name, 1),
    sideChatOf: parent.id,
    olderSettled: true as const,
  });
  const side = sideChat("focus-side");
  const otherSide = sideChat("focus-other-side");
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([parent, side, otherSide]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const parentRef = scopeThreadRef(ENVIRONMENT_ID, parent.id);
  useRightPanelStore.getState().openThread(parentRef, otherSide.id);
  useRightPanelStore.getState().openThread(parentRef, side.id);
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${parent.id}`] }),
  );
  await router.load();
  await using screen = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  using _focus = { [Symbol.dispose]: startWorkspaceFocus(router) };
  const mainEditor = screen.getByRole("textbox", { name: `Message ${parent.title}`, exact: true });
  const sideEditor = screen.getByRole("textbox", { name: `Message ${side.title}`, exact: true });
  await expect.element(sideEditor).toBeVisible();
  await userEvent.click(mainEditor);
  await userEvent.keyboard("main draft");

  await userEvent.click(screen.getByRole("button", { name: otherSide.title, exact: true }));
  const otherEditor = screen.getByRole("textbox", {
    name: `Message ${otherSide.title}`,
    exact: true,
  });
  await expect.element(otherEditor).toHaveFocus();
  await userEvent.keyboard("other draft");
  await expect.element(otherEditor).toHaveTextContent("other draft");

  await userEvent.click(screen.getByRole("button", { name: side.title, exact: true }));
  await expect.element(sideEditor).toHaveFocus();
  await expect.element(mainEditor).toHaveTextContent("main draft");
});

it("a subagent tab shows its read-only bar under no header of its own", async () => {
  const parent = threadWithHistory("subagent-parent", 1);
  const subagent = {
    ...threadWithHistory("subagent-child", 1),
    providerSubagentOf: parent.id,
    olderSettled: true as const,
  };
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([parent, subagent]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const parentRef = scopeThreadRef(ENVIRONMENT_ID, parent.id);
  useRightPanelStore.getState().openThread(parentRef, subagent.id);
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${parent.id}`] }),
  );
  await router.load();
  await using screen = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const pane = screen.getByTestId("side-thread-pane");
  // No header of its own: the tab names it, and only a side chat has actions to put in one.
  await expect.element(pane.getByRole("heading", { name: subagent.title })).not.toBeInTheDocument();
  await expect.element(pane.getByText("Runs on its own")).toBeVisible();
  await expect.element(pane.getByRole("button", { name: "Add to chat" })).not.toBeInTheDocument();
  await expect
    .element(screen.getByRole("textbox", { name: `Message ${subagent.title}`, exact: true }))
    .not.toBeInTheDocument();
  await expect
    .element(screen.getByRole("textbox", { name: `Message ${parent.title}`, exact: true }))
    .toBeVisible();
});

it("/feedback with a pending reply runs as feedback and keeps the reply when the upload fails", async () => {
  const history = threadWithHistory("feedback-reply", 1);
  const at = DateTime.makeUnsafe("2026-03-17T19:12:28.000Z");
  // Without an active provider thread, `/feedback` asks for a first message instead.
  const providerThread: OrchestrationV2ProviderThread = {
    id: ProviderThreadId.make("provider-thread-feedback"),
    driver: ProviderDriverKind.make("codex"),
    providerInstanceId: ProviderInstanceId.make("codex"),
    providerSessionId: null,
    appThreadId: history.id,
    ownerNodeId: null,
    nativeThreadRef: null,
    nativeConversationHeadRef: null,
    status: "idle",
    firstRunOrdinal: 1,
    lastRunOrdinal: 1,
    handoffIds: [],
    forkedFrom: null,
    pendingBackgroundTasks: [],
    contextUsage: null,
    nativeMetadata: null,
    goal: null,
    createdAt: at,
    updatedAt: at,
  };
  const thread = { ...history, providerThreads: [providerThread] };
  // The upload queues behind the thread's visit command, which must be answered.
  using _environment = installFakeEnvironment([thread], { acceptCommands: true });
  const uploads: ProviderUploadFeedbackInput[] = [];
  using _upload = answerRpc(
    WS_METHODS.providerUploadFeedback,
    (input: ProviderUploadFeedbackInput) => {
      uploads.push(input);
      return Effect.fail(new ProviderUploadFeedbackError({ threadId: input.threadId }));
    },
  );
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  await page.viewport(1400, 900);
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText(`Answer 0 for ${thread.id}.`)).toBeVisible();
  await page.getByRole("button", { name: "Reply to this response" }).first().click();
  const replyBar = page.getByText("Replying to", { exact: true });
  await expect.element(replyBar).toBeVisible();
  const running = { ...thread.runs[0]!, status: "running" as const, completedAt: null };
  await act(() => emitThreadEvent(thread.id, { type: "run.updated", payload: running }));
  await expect.element(page.getByRole("button", { name: "Stop generation" })).toBeVisible();

  const editor = page.getByTestId("composer-editor");
  await userEvent.click(editor);
  await insertText("/feedback slow");
  // Pressing it runs feedback, so it does not offer to steer the running turn.
  await expect.element(page.getByRole("button", { name: "Submit message" })).toBeVisible();
  await userEvent.keyboard("{Enter}");

  await vi.waitFor(() => expect(uploads).toEqual([{ threadId: thread.id, reason: "slow" }]));
  await expect
    .element(page.getByText("Could not send feedback to OpenAI", { exact: false }).first())
    .toBeVisible();
  await expect.element(replyBar).toBeVisible();
  expect(editor.element().textContent).toBe("");
});

// What the /side tests share: a thread with a reply, another thread to leave to, and uploads and
// forks the test can hold.
async function openSideChatThread(name: string) {
  await using stack = new AsyncDisposableStack();
  await page.viewport(1400, 900);
  const thread = threadWithHistory(name, 1);
  const other = threadWithHistory(`${name}-other`, 1);
  const held = { fork: false, upload: null as Promise<void> | null, uploadsStarted: 0 };
  stack.use(
    installFakeEnvironment([thread, other], {
      acceptCommands: true,
      holdCommand: (command) => held.fork && command.type === "thread.fork",
    }),
  );
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  stack.use(
    await render(<AppRoot router={router} />, {
      // AppRoot brings its own router.
      router: false,
    }),
  );
  stack.use(
    acceptAttachmentUploads(() => {
      held.uploadsStarted += 1;
      return held.upload ?? Promise.resolve();
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${thread.id}.`)).toBeVisible();

  const editor = page.getByRole("textbox", { name: `Message ${thread.title}`, exact: true });
  const image = page.getByRole("img", { name: "shot.png" });
  const pasteImage = async () => {
    await userEvent.click(editor);
    const clipboardData = new DataTransfer();
    clipboardData.items.add(new File([PNG], "shot.png", { type: "image/png" }));
    editor
      .element()
      .dispatchEvent(
        new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
      );
    await expect.element(image).toBeVisible();
  };
  return {
    mounted: stack.move(),
    thread,
    other,
    held,
    editor,
    image,
    draftTarget: scopeThreadRef(ENVIRONMENT_ID, thread.id),
    pasteImage,
    sendImageToSideChat: async () => {
      await pasteImage();
      await insertText("/side what is this?");
      await userEvent.keyboard("{Enter}");
    },
    // Holds the uploads that start from now on, and returns what lets them finish.
    holdUploads: () => {
      let release = () => {};
      held.upload = new Promise((resolve) => {
        release = resolve;
      });
      return () => {
        release();
        held.upload = null;
      };
    },
    sentMessage: (fields: Record<string, unknown>) =>
      vi.waitFor(() =>
        expect(dispatchedCommands()).toContainEqual(
          expect.objectContaining({ type: "message.dispatch", ...fields }),
        ),
      ),
    forks: () => dispatchedCommands().filter((command) => command.type === "thread.fork"),
    open: async (threadId: ThreadId, text: string) => {
      await act(() =>
        router.navigate({
          to: "/$environmentId/$threadId",
          params: { environmentId: ENVIRONMENT_ID, threadId },
        }),
      );
      await expect.element(page.getByText(text).first()).toBeVisible();
    },
    openSettings: () => act(() => router.navigate({ to: "/settings/general" })),
  };
}

const DRAFT_CHANGED = "Draft changed while preparing this message. Review it and send again.";

it("/side sends the draft's attachments and context, and keeps them until it starts", async () => {
  const side = await openSideChatThread("side-attachments");
  await using _mounted = side.mounted;
  const { thread, other, held, editor, image, draftTarget, sentMessage, forks } = side;

  await side.sendImageToSideChat();
  await sentMessage({
    text: "what is this?",
    attachments: [expect.objectContaining({ type: "image", id: "upload-1" })],
    context: {
      version: 1,
      records: [expect.objectContaining({ kind: "image", attachmentId: "upload-1" })],
    },
  });
  await expect.element(image).not.toBeInTheDocument();

  // A reply alone is enough to send.
  await page.getByRole("button", { name: "Reply to this response" }).first().click();
  await userEvent.click(editor);
  await insertText("/side ");
  await userEvent.keyboard("{Enter}");
  await sentMessage({
    context: expect.objectContaining({ records: [expect.objectContaining({ kind: "response" })] }),
  });
  // The tab opens once the start finished; until then `/side` refuses another side chat.
  await expect
    .element(page.getByRole("button", { name: "Side chat", exact: true }))
    .toHaveLength(2);

  // While the upload runs the draft keeps showing it. The side chat takes the mode the
  // composer had at Send, and starts from its thread after the user leaves.
  useComposerDraftStore.getState().setRuntimeMode(draftTarget, "approval-required");
  const releaseUpload = side.holdUploads();
  const startedBefore = held.uploadsStarted;
  await side.sendImageToSideChat();
  await vi.waitFor(() => expect(held.uploadsStarted).toBeGreaterThan(startedBefore));
  await expect.element(image).toBeVisible();
  useComposerDraftStore.getState().setRuntimeMode(draftTarget, "full-access");
  await side.open(other.id, `Answer 0 for ${other.id}.`);
  releaseUpload();
  await vi.waitFor(() => expect(forks()).toHaveLength(3));
  const fork = forks()[2]!;
  expect(fork).toMatchObject({ sourceThreadId: thread.id });
  await vi.waitFor(() =>
    expect(dispatchedCommands()).toContainEqual(
      expect.objectContaining({
        type: "thread.runtime-mode.set",
        threadId: fork.type === "thread.fork" ? fork.targetThreadId : null,
        runtimeMode: "approval-required",
      }),
    ),
  );
  await sentMessage({ threadId: fork.type === "thread.fork" ? fork.targetThreadId : null });
  await side.open(thread.id, `Answer 0 for ${thread.id}.`);
  await expect.element(image).not.toBeInTheDocument();
  await expect.element(editor).not.toHaveTextContent("/side");
});

it("/side keeps a question whose start failed or whose draft changed for the next Send", async () => {
  const side = await openSideChatThread("side-kept");
  await using _mounted = side.mounted;
  const { held, editor, image, sentMessage, forks } = side;

  // A failed start leaves the question where it was, so sending again starts a side chat.
  held.fork = true;
  await side.sendImageToSideChat();
  await vi.waitFor(() => expect(forks()).toHaveLength(1));
  await expect.element(image).toBeVisible();
  await insertText(" next");
  await rejectPendingCommand("Side chats are paused.");
  await expect.element(image).toBeVisible();
  await vi.waitFor(() =>
    expect((editor.element() as HTMLElement).innerText.trim()).toBe("/side what is this? next"),
  );
  held.fork = false;
  await userEvent.keyboard("{Enter}");
  await sentMessage({ text: "what is this? next" });
  await expect.element(image).not.toBeInTheDocument();
  await expect.element(page.getByText("Side chats are paused.")).not.toBeInTheDocument();

  // A change before the fork, such as removing an attachment and its upload, waits for Send.
  const releaseUpload = side.holdUploads();
  const uploadsBeforeEdit = held.uploadsStarted;
  await side.sendImageToSideChat();
  await vi.waitFor(() => expect(held.uploadsStarted).toBeGreaterThan(uploadsBeforeEdit));
  await page.getByRole("button", { name: "Remove shot.png" }).click();
  releaseUpload();
  await expect.element(page.getByText(DRAFT_CHANGED)).toBeVisible();
  expect(forks()).toHaveLength(2);
  await expect.element(image).not.toBeInTheDocument();
});

it("/side says where the user is that a start failed after they left its thread", async () => {
  const side = await openSideChatThread("side-left");
  await using _mounted = side.mounted;
  const { thread, other, held, editor, image, forks } = side;

  // A draft changed while the user was away is not sent.
  const releaseUpload = side.holdUploads();
  await side.sendImageToSideChat();
  await vi.waitFor(() => expect(held.uploadsStarted).toBeGreaterThan(0));
  await side.open(other.id, `Answer 0 for ${other.id}.`);
  useComposerDraftStore.getState().setPrompt(side.draftTarget, "/side what is this? more");
  releaseUpload();
  await expect.element(page.getByText(DRAFT_CHANGED)).toBeVisible();
  expect(forks()).toHaveLength(0);

  // A start that fails after the user left says so where they are.
  await side.open(thread.id, `Answer 0 for ${thread.id}.`);
  await expect.element(image).toBeVisible();
  await userEvent.click(editor);
  held.fork = true;
  await userEvent.keyboard("{Enter}");
  await vi.waitFor(() => expect(forks()).toHaveLength(1));
  await side.open(other.id, `Answer 0 for ${other.id}.`);
  await rejectPendingCommand("Side chats are paused.");
  await expect.element(page.getByText("Side chats are paused.")).toBeVisible();

  // So does one that fails while Settings covers its thread.
  await side.open(thread.id, `Answer 0 for ${thread.id}.`);
  await userEvent.click(editor);
  await userEvent.keyboard("{Enter}");
  await vi.waitFor(() => expect(forks()).toHaveLength(2));
  await side.openSettings();
  await expect
    .element(page.getByRole("heading", { name: "Time format", exact: true }))
    .toBeVisible();
  await rejectPendingCommand("Side chats are full.");
  await vi.waitFor(() =>
    expect(
      [...document.querySelectorAll("[data-slot=toast-description]")].map(
        (toast) => toast.textContent,
      ),
    ).toContain("Side chats are full."),
  );
});

it("changing the mode cancels a send held for a compressing image, and the next send uses it", async () => {
  await page.viewport(1400, 900);
  const thread = threadWithHistory("held-send-mode", 1);
  using _environment = installFakeEnvironment([thread], { acceptCommands: true });
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftTarget = scopeThreadRef(ENVIRONMENT_ID, thread.id);
  const targetKey = composerTargetKey(draftTarget);
  using _preparing = { [Symbol.dispose]: () => cancelPreparingComposerImages(targetKey) };
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${thread.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText(`Answer 0 for ${thread.id}.`)).toBeVisible();

  await act(() =>
    useComposerDraftStore.getState().setRuntimeMode(draftTarget, "approval-required"),
  );
  const editor = page.getByTestId("composer-editor");
  await userEvent.click(editor);
  await insertText("Held prompt");
  const compressing = {
    id: "compressing",
    name: "big.png",
    file: new File([PNG], "big.png", { type: "image/png" }),
    hidden: false,
  };
  await act(() => addPreparingComposerImages(targetKey, [compressing]));
  await userEvent.keyboard("{Enter}");
  await expect.element(page.getByText("Sending once image is ready")).toBeVisible();
  await act(() => useComposerDraftStore.getState().setRuntimeMode(draftTarget, "full-access"));
  await expect.element(page.getByText("Sending once image is ready")).not.toBeInTheDocument();
  await act(() => removePreparingComposerImages(targetKey, new Set([compressing.id])));
  await expect.element(editor).toHaveTextContent("Held prompt");

  await userEvent.keyboard("{Enter}");
  await vi.waitFor(() =>
    expect(dispatchedCommands()).toContainEqual(
      expect.objectContaining({ type: "message.dispatch", text: "Held prompt" }),
    ),
  );
  expect(
    dispatchedCommands().filter((command) => command.type === "message.dispatch"),
  ).toHaveLength(1);
  expect(dispatchedCommands()).not.toContainEqual(
    expect.objectContaining({ type: "thread.runtime-mode.set", runtimeMode: "approval-required" }),
  );
});

it("opening Settings without a page event closes the composer model picker and permissions menu", async () => {
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftId = DraftId.make("draft-29219");
  useComposerDraftStore
    .getState()
    .setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
      threadId: ThreadId.make("thread-29219"),
    });

  const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByTestId("composer-editor")).toBeVisible();
  const trigger = document.querySelector<HTMLElement>("[data-chat-provider-model-picker]");
  if (!trigger) throw new Error("composer model picker trigger missing");
  await userEvent.click(trigger);
  const search = page.getByPlaceholder("Search models");
  await expect.element(search).toBeVisible();

  // The desktop menu's open-settings accelerator navigates with no key or pointer event in the page.
  await act(() => router.navigate({ to: "/settings" }));
  await expect.element(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
  await expect.element(search).not.toBeInTheDocument();

  await act(() => router.navigate({ to: "/draft/$draftId", params: { draftId } }));
  await page.getByRole("button", { name: /^Permissions: / }).click();
  // The modal Settings dialog aria-hides everything outside it, so match text, not role.
  const permissions = page.getByText("Allow commands and edits without prompts.").last();
  await expect.element(permissions).toBeVisible();
  await act(() => router.navigate({ to: "/settings" }));
  await expect.element(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
  await expect.element(permissions).not.toBeInTheDocument();
});

it("a send held for onicode.json says it was not sent once the user leaves its draft", async () => {
  await page.viewport(1400, 900);
  const other = threadWithHistory("workspace-default-left", 1);
  using _environment = installFakeEnvironment([other], { acceptCommands: true });
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  using projectFile = holdRpc(
    WS_METHODS.projectsReadFile,
    Effect.succeed({
      relativePath: "onicode.json",
      contents: "",
      byteLength: 0,
      truncated: false,
      exists: false,
    }),
  );
  let launches = 0;
  using _launch = answerRpc(ORCHESTRATION_V2_WS_METHODS.launchThread, () => {
    launches += 1;
    return Effect.never;
  });
  const router = getRouter(
    createMemoryHistory({ initialEntries: [`/${ENVIRONMENT_ID}/${other.id}`] }),
  );
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  await expect.element(page.getByText(`Answer 0 for ${other.id}.`)).toBeVisible();
  // A new draft needs the project, which arrives with the shell snapshot.
  await expect.element(page.getByText("render", { exact: true }).first()).toBeVisible();
  // An earlier test's refused connection can leave this read retrying; start it on this socket.
  appAtomRegistry.refresh(
    getProjectFileQueryAtom(ENVIRONMENT_ID, "/workspace/render", "onicode.json"),
  );
  await projectFile.called;
  const opened = await act(() =>
    createNewThreadHandler(router)(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID)),
  );
  if (!opened) throw new Error("No draft opened");
  // The kept draft would otherwise list in the next test's sidebar.
  using _draft = {
    [Symbol.dispose]: () => useComposerDraftStore.getState().clearDraftThread(opened.draftId),
  };

  const editor = page.getByTestId("composer-editor");
  await userEvent.click(editor);
  await insertText("Held for the project file");
  await userEvent.keyboard("{Enter}");
  await expect.element(page.getByRole("button", { name: "Submitting message" })).toBeVisible();
  // The composer on screen is now this thread's, which the held send must not read.
  await act(() =>
    router.navigate({
      to: "/$environmentId/$threadId",
      params: { environmentId: ENVIRONMENT_ID, threadId: other.id },
    }),
  );
  await expect.element(page.getByText(`Answer 0 for ${other.id}.`)).toBeVisible();
  projectFile.release();

  await expect.element(page.getByText("Message not sent")).toBeVisible();
  expect(launches).toBe(0);
  expect(useComposerDraftStore.getState().getComposerDraft(opened.draftId)?.prompt).toBe(
    "Held for the project file",
  );
});

it("a new thread on a branch with changes opens them from thread details", async () => {
  // Wide enough that thread details sit inline, where they start open.
  await page.viewport(1400, 900);
  using _environment = installFakeEnvironment([]);
  using _status = answerRpc(WS_METHODS.subscribeVcsStatus, () =>
    Stream.concat(
      Stream.make({
        _tag: "snapshot" as const,
        local: {
          isRepo: true,
          hasPrimaryRemote: false,
          isDefaultRef: false,
          refName: "feature",
          hasWorkingTreeChanges: false,
          workingTree: { files: [], insertions: 0, deletions: 0 },
          branchChanges: { baseRef: "main", insertions: 150, deletions: 97 },
        },
        remote: null,
      }),
      Stream.never,
    ),
  );
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    onboardingCompletedAt: "2026-01-01T00:00:00.000Z",
  });
  const draftId = DraftId.make("draft-43905");
  useComposerDraftStore
    .getState()
    .setProjectDraftThreadId(scopeProjectRef(ENVIRONMENT_ID, PROJECT_ID), draftId, {
      threadId: ThreadId.make("thread-43905"),
      // Its own checkout, so the status subscription is this test's and not an earlier one's.
      worktreePath: "/workspace/render-43905",
    });

  const router = getRouter(createMemoryHistory({ initialEntries: [`/draft/${draftId}`] }));
  await router.load();
  await using _app = await render(<AppRoot router={router} />, {
    // AppRoot brings its own router.
    router: false,
  });
  const changesRow = page
    .getByRole("region", { name: "Thread details" })
    .getByRole("button", { name: /^Changes\s*vs main\s*\+150/ });
  // The row already shows the branch's totals, so it has to open them.
  await expect.element(changesRow).toBeEnabled();

  await changesRow.click();
  // The panel reads the draft's checkout; without one it asks for a thread instead.
  await expect.element(page.getByText("Loading changes…")).toBeVisible();
});
