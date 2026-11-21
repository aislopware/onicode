import { assert, it } from "@effect/vitest";
import { IssueTrackerKind, ProjectId } from "@onicode/contracts";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import * as IssueTrackerBindings from "../IssueTrackerBindings.ts";
import { runMigrations } from "../Migrations.ts";

it.layer(NodeSqliteClient.layer({ filename: ":memory:" }))(
  "108_IssueTrackerAdditionalScopes",
  (it) => {
    it.effect("keeps a one-team binding and saves, replaces, keeps and clears extra teams", () =>
      Effect.gen(function* () {
        const sql = yield* SqlClient.SqlClient;
        yield* runMigrations({ toMigrationInclusive: 107 });
        yield* sql`INSERT INTO issue_tracker_bindings VALUES ('web', 'linear', 'ENG', '2026-11-21')`;
        yield* runMigrations();
        const bindings = yield* IssueTrackerBindings.IssueTrackerBindingsRepository.pipe(
          Effect.provide(IssueTrackerBindings.layer),
        );
        const projectId = ProjectId.make("web");
        const binding = { tracker: IssueTrackerKind.make("linear"), scope: "ENG" };
        const read = () => bindings.list.pipe(Effect.map((all) => all.get(projectId)));
        assert.deepStrictEqual(yield* read(), binding);
        const multi = { ...binding, additionalScopes: ["OPS", "DOCS"] };
        yield* bindings.set({ projectId, binding: multi, updatedAt: "2026-11-21" });
        assert.deepStrictEqual(yield* read(), multi);
        // Clients that predate extra teams omit them, as when re-selecting or switching the team.
        yield* bindings.set({ projectId, binding, updatedAt: "2026-11-21" });
        assert.deepStrictEqual(yield* read(), multi);
        const switched = { ...binding, scope: "OPS" };
        yield* bindings.set({ projectId, binding: switched, updatedAt: "2026-11-21" });
        assert.deepStrictEqual(yield* read(), { ...switched, additionalScopes: ["DOCS"] });
        yield* bindings.set({
          projectId,
          binding: { ...binding, additionalScopes: [] },
          updatedAt: "2026-11-21",
        });
        assert.deepStrictEqual(yield* read(), binding);
      }),
    );
  },
);
