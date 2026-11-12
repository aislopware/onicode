import { bindings, defineConfig } from "cf/config";

import { loadRepoEnv, resolveMobileIdentity } from "../../scripts/lib/public-config.ts";

// The gateway serves the app the mobile build is, so both read the same settings.
const identity = resolveMobileIdentity(loadRepoEnv());
// The development variant keeps this id in every checkout. Only its owners can sign it for a
// phone, so another app's deployed gateway leaves it out and serves it to simulators locally.
const DEVELOPMENT_APP_ID = "dev.aislopware.onicode.dev";

const perMinute = (namespace: string, limit: number) =>
  bindings.rateLimit({ namespace, simple: { limit, period: 60 } });

export default defineConfig(({ mode }) => {
  // `cf dev` reads only declared secrets from .dev.vars, so locally that file decides what is served.
  const local = mode === "development";
  return {
    worker: {
      name: "onicode-push-gateway",
      compatibilityDate: "2026-11-01",
      entrypoint: "./src/index.ts",
      observability: { enabled: true },
      // No preview or workers.dev URL: both skip the custom domain's protections.
      workersDev: false,
      previewUrls: false,
      domains:
        identity.pushGatewayUrl === undefined ? [] : [new URL(identity.pushGatewayUrl).hostname],
      env: {
        // A deploy fails while a secret named here has no value.
        PUSH_SEALING_KEYS: bindings.secret(),
        ...(identity.appleTeamId === undefined
          ? {}
          : {
              APNS_TEAM_ID: bindings.text(identity.appleTeamId),
              IOS_BUNDLE_IDS: bindings.text(
                local || !identity.own ? `${identity.appId},${DEVELOPMENT_APP_ID}` : identity.appId,
              ),
              APNS_KEY_ID: bindings.secret(),
              APNS_PRIVATE_KEY: bindings.secret(),
            }),
        ...(local || identity.androidGoogleServicesFile !== undefined
          ? {
              ANDROID_BOOT_POLICY: bindings.text("verified"),
              ANDROID_APPS: bindings.secret(),
              FCM_SERVICE_ACCOUNT: bindings.secret(),
            }
          : {}),
        ...(local ? { UNATTESTED_APP_IDS: bindings.secret() } : {}),
        // Counted per Cloudflare location and enforced loosely: a burst overshoots before refusals
        // start. Namespace ids only need to be unique in the account.
        RL_CHALLENGE: perMinute("4101", 20),
        RL_ATTEST: perMinute("4102", 5),
        RL_GRANTS: perMinute("4103", 30),
        RL_DELIVER_DEVICE: perMinute("4104", 60),
        RL_DELIVER_ENVIRONMENT: perMinute("4105", 600),
      },
    },
  };
});
