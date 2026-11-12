/// <reference types="@cloudflare/workers-types" />
import type { GatewayEnv } from "./Config.ts";
import type { RateLimitBinding } from "./RateLimiter.ts";

declare global {
  namespace Cloudflare {
    interface Env extends GatewayEnv {
      readonly RL_CHALLENGE: RateLimitBinding;
      readonly RL_ATTEST: RateLimitBinding;
      readonly RL_GRANTS: RateLimitBinding;
      readonly RL_DELIVER_DEVICE: RateLimitBinding;
      readonly RL_DELIVER_ENVIRONMENT: RateLimitBinding;
    }
  }
}
