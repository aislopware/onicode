import { PushGatewayUnavailableError } from "@onicode/contracts/pushGateway";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as HttpClient from "effect/http/HttpClient";
import * as HttpClientResponse from "effect/http/HttpClientResponse";

const STATUS_URL = "https://android.googleapis.com/attestation/status";
// Google serves the list with a one-day max-age.
const REFRESH_AFTER_MS = 24 * 60 * 60_000;
// While Google is down, every Android attestation would otherwise wait on its own failing fetch.
const RETRY_AFTER_MS = 60_000;

const StatusList = Schema.Struct({ entries: Schema.Record(Schema.String, Schema.Unknown) });

/** Serial numbers of attestation certificates Google revoked, mostly leaked device keys. */
export class AndroidRevocations extends Context.Service<
  AndroidRevocations,
  { readonly serials: Effect.Effect<ReadonlySet<string>, PushGatewayUnavailableError> }
>()("@onicode/push-gateway/attestation/AndroidRevocations") {}

export const layer = Layer.effect(
  AndroidRevocations,
  Effect.gen(function* () {
    const client = yield* HttpClient.HttpClient;
    let cached: { readonly serials: ReadonlySet<string>; readonly fetchedAt: number } | undefined;
    let failedAt: number | undefined;

    const fetchSerials = client.get(STATUS_URL).pipe(
      Effect.flatMap(HttpClientResponse.filterStatusOk),
      Effect.flatMap(HttpClientResponse.schemaBodyJson(StatusList)),
      Effect.map((list) => new Set(Object.keys(list.entries))),
      Effect.timeout("10 seconds"),
    );

    return AndroidRevocations.of({
      serials: Effect.gen(function* () {
        const now = yield* Clock.currentTimeMillis;
        if (cached !== undefined && now - cached.fetchedAt < REFRESH_AFTER_MS) {
          return cached.serials;
        }
        // Without any list a revoked key would pass, so only an older list may stand in.
        const withoutFetch =
          cached === undefined
            ? Effect.fail(new PushGatewayUnavailableError({ reason: "dependency-unavailable" }))
            : Effect.succeed(cached.serials);
        if (failedAt !== undefined && now - failedAt < RETRY_AFTER_MS) return yield* withoutFetch;
        return yield* fetchSerials.pipe(
          Effect.tap((serials) =>
            Effect.sync(() => {
              cached = { serials, fetchedAt: now };
              failedAt = undefined;
            }),
          ),
          Effect.catch((error) => {
            failedAt = now;
            return Effect.logError("Android revocation list could not be fetched", { error }).pipe(
              Effect.andThen(withoutFetch),
            );
          }),
        );
      }).pipe(Effect.withSpan("AndroidRevocations.serials")),
    });
  }),
);
