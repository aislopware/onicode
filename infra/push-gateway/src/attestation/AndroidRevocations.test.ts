import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as HttpClient from "effect/http/HttpClient";
import * as HttpClientResponse from "effect/http/HttpClientResponse";
import * as TestClock from "effect/testing/TestClock";

import * as AndroidRevocations from "./AndroidRevocations.ts";

describe("AndroidRevocations", () => {
  it.effect("asks Google at most once a minute while it is down, and once a day after", () =>
    Effect.gen(function* () {
      let status = 503;
      let requests = 0;
      const context = yield* Layer.build(
        AndroidRevocations.layer.pipe(
          Layer.provide(
            Layer.succeed(
              HttpClient.HttpClient,
              HttpClient.make((request) =>
                Effect.sync(() => {
                  requests += 1;
                  return HttpClientResponse.fromWeb(
                    request,
                    Response.json({ entries: { abc123: { status: "REVOKED" } } }, { status }),
                  );
                }),
              ),
            ),
          ),
        ),
      );
      const { serials } = yield* AndroidRevocations.AndroidRevocations.pipe(
        Effect.provide(context),
      );
      const outcome = serials.pipe(
        Effect.map((revoked) => [...revoked]),
        Effect.catch((error) => Effect.succeed(error.reason)),
      );

      // With no list a revoked key would pass, so attestation is refused, not waved through.
      assert.strictEqual(yield* outcome, "dependency-unavailable");
      assert.strictEqual(yield* outcome, "dependency-unavailable");
      assert.strictEqual(requests, 1);

      status = 200;
      yield* TestClock.adjust("1 minute");
      assert.deepStrictEqual(yield* outcome, ["abc123"]);
      assert.deepStrictEqual(yield* outcome, ["abc123"]);
      assert.strictEqual(requests, 2);

      // A day-old list stands in while a refresh fails.
      status = 503;
      yield* TestClock.adjust("1 day");
      assert.deepStrictEqual(yield* outcome, ["abc123"]);
      assert.deepStrictEqual(yield* outcome, ["abc123"]);
      assert.strictEqual(requests, 3);
    }),
  );
});
