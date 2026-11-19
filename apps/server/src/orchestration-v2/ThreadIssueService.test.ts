import { IssueTrackerKind, ThreadId } from "@onicode/contracts";
import type { StandaloneIssueTrackerApi } from "@onicode/source-control-core/server/IssueTracker";
import { describe, expect, it } from "@effect/vitest";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as SqlClient from "effect/sql/SqlClient";

import * as IssueTrackerRegistry from "../issue/IssueTrackerRegistry.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as Orchestrator from "./Orchestrator.ts";
import * as ThreadIssueService from "./ThreadIssueService.ts";

const THREAD_ID = ThreadId.make("thread-1");
const LINEAR = IssueTrackerKind.make("linear");

/** Only what reading linked threads uses: the workspace a Linear issue URL names. */
const linear = {
  kind: LINEAR,
  standalone: { host: "linear.app" },
  parseIssueUrl: (url: URL) => {
    const [, workspace, resource, key] = url.pathname.split("/");
    return url.host === "linear.app" && workspace && resource === "issue" && key
      ? { host: "linear.app", workspace, scope: "ENG", key }
      : null;
  },
} as unknown as StandaloneIssueTrackerApi;

const testLayer = (orchestrator: Partial<Orchestrator.OrchestratorV2["Service"]> = {}) =>
  ThreadIssueService.layer.pipe(
    Layer.provideMerge(
      Layer.mergeAll(
        SqlitePersistence.layerMemory,
        Layer.succeed(
          IssueTrackerRegistry.IssueTrackerRegistry,
          IssueTrackerRegistry.fromTrackers([linear]),
        ),
        Layer.mock(Orchestrator.OrchestratorV2)(orchestrator),
        Layer.succeed(
          SecureRandom.SecureRandom,
          SecureRandom.make((size) => new Uint8Array(size).fill(7)),
        ),
      ),
    ),
  );

const encodePayload = Schema.encodeEffect(Schema.fromJsonString(Schema.Unknown));

describe("ThreadIssueService", () => {
  it.effect("fails an unlink whose thread cannot be read instead of reporting it unlinked", () =>
    Effect.gen(function* () {
      const service = yield* ThreadIssueService.ThreadIssueService;
      const error = yield* service
        .unlink({
          threadId: THREAD_ID,
          key: {
            tracker: IssueTrackerKind.make("github"),
            host: "github.com",
            scope: "onicode/onicode",
            key: "7",
          },
        })
        .pipe(Effect.flip);
      expect(error).toMatchObject({ _tag: "OrchestratorProjectionError", threadId: THREAD_ID });
    }).pipe(
      Effect.provide(
        testLayer({
          getThreadShell: (threadId) =>
            Effect.fail(new Orchestrator.OrchestratorProjectionError({ threadId })),
        }),
      ),
    ),
  );

  it.effect("links and unlinks one Linear key in another workspace as its own issue", () => {
    const dispatched: Array<Record<string, unknown>> = [];
    const acme = {
      tracker: LINEAR,
      host: "linear.app",
      scope: "ENG",
      key: "ENG-7",
      workspace: "acme",
    };
    return Effect.gen(function* () {
      const service = yield* ThreadIssueService.ThreadIssueService;
      const othercorp = { ...acme, workspace: "othercorp" };
      const { workspace: _workspace, ...eng7 } = acme;
      expect(
        yield* service.link({
          threadId: THREAD_ID,
          // Named by key alone, as the agent tools do: the URL the tracker gave names the workspace.
          ref: { ...eng7, projectId: "project-1" as never },
          url: "https://linear.app/OtherCorp/issue/ENG-7",
          displayKey: "ENG-7",
          title: "Elsewhere",
          source: "agent",
        }),
      ).toEqual({ alreadyLinked: false });
      // The shell stays as given: only acme's ENG-7 is linked, so othercorp's is not.
      expect(yield* service.unlink({ threadId: THREAD_ID, key: othercorp })).toEqual({
        wasLinked: false,
      });
      expect(yield* service.unlink({ threadId: THREAD_ID, key: acme })).toEqual({
        wasLinked: true,
      });
      expect(dispatched).toMatchObject([
        { type: "thread.issue.link", workspace: "othercorp" },
        { type: "thread.issue.unlink", key: "ENG-7", workspace: "acme" },
      ]);
    }).pipe(
      Effect.provide(
        testLayer({
          getThreadShell: () =>
            Effect.succeed({
              issues: [{ ...acme, url: "https://linear.app/acme/issue/ENG-7" }],
            } as never),
          dispatch: (command) =>
            Effect.sync(() => {
              dispatched.push(command as never);
            }) as never,
        }),
      ),
    );
  });

  it.effect("tells one Linear team key in two workspaces apart by the issue's URL", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const link = (url: string, workspace?: string) => ({
        tracker: "linear",
        host: "linear.app",
        scope: "ENG",
        key: "ENG-7",
        url,
        ...(workspace === undefined ? {} : { workspace }),
      });
      const fixtures = [
        { id: "acme", url: "https://linear.app/acme/issue/ENG-7/title" },
        { id: "globex", url: "https://linear.app/Globex/issue/ENG-7" },
        { id: "unparsed", url: "not a url" },
        // Linked by an agent after Globex renamed its URL key: the link keeps the live workspace.
        { id: "renamed", url: "https://linear.app/globex-old/issue/ENG-7", workspace: "globex" },
      ];
      for (const fixture of fixtures) {
        const payload = yield* encodePayload({ issues: [link(fixture.url, fixture.workspace)] });
        yield* sql`
          INSERT INTO orchestration_v2_projection_threads (
            thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
            created_at, updated_at, archived_at, deleted_at, payload_json
          ) VALUES (
            ${fixture.id}, 'project-1', ${fixture.id}, 'codex', 'full-access', 'default',
            '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z', NULL, NULL, ${payload}
          )
        `;
      }
      const service = yield* ThreadIssueService.ThreadIssueService;
      const key = { tracker: LINEAR, host: "linear.app", scope: "ENG", key: "ENG-7" };
      const ids = (result: { threads: ReadonlyArray<{ id: string }> }) =>
        result.threads.map((thread) => thread.id).toSorted();

      expect(
        ids(
          yield* service.linkedThreads({
            ...key,
            url: "https://linear.app/globex/issue/ENG-7/title",
          }),
        ),
      ).toEqual(["globex", "renamed", "unparsed"]);
      expect(ids(yield* service.linkedThreads({ ...key, workspace: "acme" }))).toEqual([
        "acme",
        "unparsed",
      ]);
      // Older clients send no URL, and keep every match.
      expect(ids(yield* service.linkedThreads(key))).toEqual([
        "acme",
        "globex",
        "renamed",
        "unparsed",
      ]);
    }).pipe(Effect.provide(testLayer())),
  );
});
