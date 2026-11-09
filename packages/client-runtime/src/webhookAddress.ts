import type { ScheduledTaskWebhookEndpoint } from "@onicode/contracts";
import { isLocalLoopbackHost } from "@onicode/shared/hostClassification";

export interface WebhookAddress {
  readonly address: string;
  readonly copyable: boolean;
  readonly note: string | null;
}

export function webhookAddress(
  endpoint: ScheduledTaskWebhookEndpoint,
  httpBaseUrl: string | null,
): WebhookAddress {
  if (endpoint.url !== null) {
    return { address: endpoint.url, copyable: true, note: null };
  }
  if (httpBaseUrl === null) {
    return {
      address: endpoint.path,
      copyable: false,
      note: "This environment has no address to call yet. Public access gives it one on the internet.",
    };
  }
  const url = new URL(endpoint.path, httpBaseUrl);
  return {
    address: url.href,
    copyable: true,
    note: isLocalLoopbackHost(url.hostname)
      ? "Only this computer can call this address. Turn on public access in Settings → Labs to receive webhooks from the internet."
      : "Works wherever this environment's address is reachable, such as over Tailscale. Turn on public access in Settings → Labs to receive webhooks from the internet.",
  };
}
