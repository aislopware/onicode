import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";

import * as OriginCli from "./OriginCli.ts";
import { make } from "./OriginPullRequestProvider.ts";

it.effect.each([
  { failing: "listComments", fails: false },
  { failing: "listCommits", fails: true },
])("fails the activity only when its commits cannot be read ($failing)", ({ failing, fails }) =>
  Effect.gen(function* () {
    const provider = yield* make.pipe(
      Effect.provide(
        Layer.mock(OriginCli.OriginCli)({
          api: (input, schema) =>
            input.operation === failing || input.operation === "getPullRequest"
              ? Effect.fail(
                  new OriginCli.OriginCliError({
                    operation: input.operation,
                    cwd: input.cwd,
                    detail: "Origin is down.",
                    reason: "failed",
                  }),
                )
              : Schema.decodeEffect(schema)({}).pipe(Effect.orDie),
        }),
      ),
    );
    const result = yield* Effect.result(
      provider.getChangeRequestActivity({
        cwd: "/w",
        repository: "acme/web",
        host: "cursor.com",
        number: 7,
      }),
    );

    assert.strictEqual(result._tag, fails ? "Failure" : "Success");
  }),
);
