import { PushGatewayRateLimitedError } from "@onicode/contracts/pushGateway";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";

/** One Workers rate limit binding each; cloudflare.config.ts sets the limits. */
export type RateLimitBucket =
  | "challenge"
  | "attest"
  | "grants"
  | "deliverDevice"
  | "deliverEnvironment";

/** The slice of Cloudflare's `RateLimit` binding the gateway uses. */
export interface RateLimitBinding {
  readonly limit: (options: { readonly key: string }) => Promise<{ readonly success: boolean }>;
}

const PERIOD_SECONDS = 60;

/** An IPv6 subscriber holds a whole /64 and can rotate within it, so the prefix is the client. */
export function addressRateKey(address: string): string {
  if (!address.includes(":")) return address;
  const [head = "", tail] = address.toLowerCase().split("::");
  const leading = head === "" ? [] : head.split(":");
  const trailing = tail === undefined || tail === "" ? [] : tail.split(":");
  const groups =
    tail === undefined
      ? leading
      : [...leading, ...Array<string>(Math.max(0, 8 - leading.length - trailing.length)).fill("0")];
  const prefix = groups.slice(0, 4).map((group) => group.replace(/^0+(?=.)/, ""));
  return `${prefix.join(":")}::/64`;
}

class RateLimitBindingError extends Schema.TaggedError<RateLimitBindingError>()(
  "RateLimitBindingError",
  { cause: Schema.Defect() },
) {}

export class RateLimiter extends Context.Service<
  RateLimiter,
  {
    readonly check: (
      bucket: RateLimitBucket,
      key: string,
    ) => Effect.Effect<void, PushGatewayRateLimitedError>;
  }
>()("@onicode/push-gateway/RateLimiter") {}

export const layer = (bindings: Readonly<Record<RateLimitBucket, RateLimitBinding>>) =>
  Layer.succeed(
    RateLimiter,
    RateLimiter.of({
      check: Effect.fn("RateLimiter.check")(function* (bucket, key) {
        // A limiter outage must not stop pushes: the sealed tokens still gate every request.
        const { success } = yield* Effect.tryPromise({
          try: () => bindings[bucket].limit({ key }),
          catch: (cause) => new RateLimitBindingError({ cause }),
        }).pipe(
          Effect.catch((error) =>
            Effect.as(Effect.logError("Rate limit binding failed", { bucket, error }), {
              success: true,
            }),
          ),
        );
        if (!success) {
          return yield* new PushGatewayRateLimitedError({ retryAfterSeconds: PERIOD_SECONDS });
        }
      }),
    }),
  );
