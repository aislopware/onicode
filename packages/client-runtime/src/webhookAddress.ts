import type { ScheduledTaskWebhookEndpoint } from "@onicode/contracts";
import { isLocalLoopbackHost } from "@onicode/shared/hostClassification";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

export interface WebhookAddress {
  readonly address: string;
  readonly copyable: boolean;
  readonly note: string | null;
}

export function webhookAddress(
  endpoint: ScheduledTaskWebhookEndpoint,
  httpBaseUrl: string | null,
  resolve: MFunctionType = mFallback,
): WebhookAddress {
  if (endpoint.url !== null) {
    return { address: endpoint.url, copyable: true, note: null };
  }
  if (httpBaseUrl === null) {
    return {
      address: endpoint.path,
      copyable: false,
      note: resolve(
        msg(
          "This environment has no address to call yet. Turn on public webhooks under Public access in Settings for a public URL.",
        ),
      ),
    };
  }
  const url = new URL(endpoint.path, httpBaseUrl);
  return {
    address: url.href,
    copyable: true,
    note: isLocalLoopbackHost(url.hostname)
      ? resolve(
          msg(
            "Only this computer can call this address. Turn on public webhooks under Public access in Settings for a public URL.",
          ),
        )
      : resolve(
          msg(
            "Works wherever this environment's address is reachable, for example over Tailscale. Turn on public webhooks under Public access in Settings for a public URL.",
          ),
        ),
  };
}
