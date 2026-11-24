import * as NodeDns from "node:dns";
import * as NodeNet from "node:net";

import * as NodeHttpClient from "@effect/platform-node/NodeHttpClient";
import { isTailnetHost, normalizeHostname } from "@onicode/shared/hostClassification";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

/**
 * Extra hosts that may receive webhooks although they are loopback or plain `http:` (for example
 * `127.0.0.1` for a local receiver in tests). Comma-separated host names or addresses.
 */
export const ALLOWED_HOSTS_ENV = "ONICODE_MCP_WEBHOOK_ALLOWED_HOSTS";

export const allowedHosts = Config.String(ALLOWED_HOSTS_ENV).pipe(
  Config.withDefault(""),
  Config.map((value) => value.split(",").filter((host) => host.trim() !== "")),
);

class WebhookAddressRejected extends Schema.TaggedError<WebhookAddressRejected>()(
  "WebhookAddressRejected",
  { message: Schema.String },
) {}

// IANA special-purpose ranges that are not globally reachable. Separate IPv4 and IPv6 lists avoid
// matching IPv4 addresses against IPv4-mapped IPv6 rules.
const notGlobalV4 = new NodeNet.BlockList();
for (const [network, prefix] of [
  ["0.0.0.0", 8],
  ["10.0.0.0", 8],
  ["100.64.0.0", 10],
  ["127.0.0.0", 8],
  ["169.254.0.0", 16],
  ["172.16.0.0", 12],
  ["192.0.0.0", 24],
  ["192.0.2.0", 24],
  ["192.88.99.0", 24],
  ["192.168.0.0", 16],
  ["198.18.0.0", 15],
  ["198.51.100.0", 24],
  ["203.0.113.0", 24],
  ["224.0.0.0", 4],
  ["240.0.0.0", 4],
] as const) {
  notGlobalV4.addSubnet(network, prefix, "ipv4");
}
const globalUnicastV6 = new NodeNet.BlockList();
globalUnicastV6.addSubnet("2000::", 3, "ipv6");
const notGlobalV6 = new NodeNet.BlockList();
for (const [network, prefix] of [
  ["2001::", 23],
  ["2001:db8::", 32],
  ["2002::", 16],
  ["3fff::", 20],
] as const) {
  notGlobalV6.addSubnet(network, prefix, "ipv6");
}

// Private/LAN (RFC 1918) and Tailscale/CGNAT (RFC 6598) IPv4 ranges; loopback and link-local stay blocked.
const privateOrTailnetV4 = new NodeNet.BlockList();
for (const [network, prefix] of [
  ["10.0.0.0", 8],
  ["100.64.0.0", 10],
  ["172.16.0.0", 12],
  ["192.168.0.0", 16],
] as const) {
  privateOrTailnetV4.addSubnet(network, prefix, "ipv4");
}
// RFC 4193 Unique Local Addresses, including Tailscale IPv6 (fd7a:115c:a1e0::/48).
const privateOrTailnetV6 = new NodeNet.BlockList();
privateOrTailnetV6.addSubnet("fc00::", 7, "ipv6");

const isGloballyRoutable = (address: string): boolean => {
  const family = NodeNet.isIP(address);
  if (family === 4) return !notGlobalV4.check(address, "ipv4");
  return (
    family === 6 && globalUnicastV6.check(address, "ipv6") && !notGlobalV6.check(address, "ipv6")
  );
};

// Cloud metadata services inside those ranges: Alibaba's, and AWS's IPv6 IMDS. One list on purpose,
// so the IPv4 rule also matches its IPv4-mapped IPv6 form.
const cloudMetadata = new NodeNet.BlockList();
cloudMetadata.addAddress("100.100.100.200", "ipv4");
cloudMetadata.addSubnet("fd00:ec2::", 32, "ipv6");

export const isPrivateOrTailnetAddress = (address: string): boolean => {
  const family = NodeNet.isIP(address);
  if (family !== 0 && cloudMetadata.check(address, family === 4 ? "ipv4" : "ipv6")) return false;
  if (family === 4) return privateOrTailnetV4.check(address, "ipv4");
  return family === 6 && privateOrTailnetV6.check(address, "ipv6");
};

/** Publicly routable, private/LAN, or Tailscale addresses; excludes loopback, link-local and reserved ranges. */
export const isAllowedAddress = (address: string): boolean =>
  isGloballyRoutable(address) || isPrivateOrTailnetAddress(address);

const hostOf = (url: URL) => normalizeHostname(url.hostname);

const resolveAll = (host: string) =>
  Effect.callback<ReadonlyArray<string>>((resume) => {
    NodeDns.lookup(host, { all: true }, (error, addresses) =>
      resume(Effect.succeed(error === null ? addresses.map((resolved) => resolved.address) : [])),
    );
  });

/**
 * The webhook address check and HTTP client deliveries go through. Onicode runs self-hosted, so
 * private/LAN IPs and Tailscale (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, `*.ts.net`) are allowed
 * alongside public `https:` endpoints, while loopback, link-local/metadata, non-http(s) schemes,
 * embedded URL credentials, and redirects remain rejected.
 */
export const make = (
  hosts: ReadonlyArray<string>,
  options?: {
    readonly resolve?: (host: string) => Effect.Effect<ReadonlyArray<string>>;
  },
) => {
  const allowed = new Set(hosts.map((host) => normalizeHostname(host.trim())));
  const resolve = options?.resolve ?? resolveAll;
  const allowsPlaintext = (url: URL) => {
    const host = hostOf(url);
    return allowed.has(host) || isPrivateOrTailnetAddress(host) || isTailnetHost(host);
  };
  const check = Effect.fn("McpWebhookTransport.check")(function* (url: URL) {
    if (url.username !== "" || url.password !== "") {
      return yield* new WebhookAddressRejected({
        message: "Webhook URL must not carry credentials",
      });
    }
    const host = hostOf(url);
    if (url.protocol !== "https:" && !(url.protocol === "http:" && allowsPlaintext(url))) {
      return yield* new WebhookAddressRejected({
        message: `${url.protocol} is not allowed for ${host}`,
      });
    }
    if (allowed.has(host)) return;
    const addresses =
      NodeNet.isIP(host) !== 0
        ? [host]
        : Option.getOrElse(
            yield* resolve(host).pipe(
              // A subscribe holds its slot while this runs, and a delivery's request timeout
              // does not cover it, so a slow resolver counts as no answer.
              Effect.timeoutOption("5 seconds"),
            ),
            () => [] as ReadonlyArray<string>,
          );
    if (addresses.length === 0) {
      return yield* new WebhookAddressRejected({ message: `${host} could not be resolved` });
    }
    if (url.protocol === "http:") {
      if (!addresses.every(isPrivateOrTailnetAddress)) {
        return yield* new WebhookAddressRejected({ message: `${host} requires https` });
      }
      return;
    }
    if (!addresses.every(isAllowedAddress)) {
      return yield* new WebhookAddressRejected({
        message: `${host} is not an allowed webhook address`,
      });
    }
  });
  const lookup: NodeNet.LookupFunction = (hostname, lookupOptions, callback) => {
    NodeDns.lookup(hostname, { ...lookupOptions, all: true }, (error, addresses) => {
      if (error !== null) return callback(error, "");
      if (
        !allowed.has(normalizeHostname(hostname)) &&
        (addresses.length === 0 || addresses.some((address) => !isAllowedAddress(address.address)))
      ) {
        return callback(
          Object.assign(new Error(`${hostname} is not an allowed webhook address`), {
            code: "ECONNREFUSED",
          }),
          "",
        );
      }
      if (lookupOptions.all === true) return callback(null, addresses);
      const [first] = addresses;
      return callback(null, first!.address, first!.family);
    });
  };
  return {
    check,
    allowsPlaintext,
    // The Undici client statically imports undici, which a single-executable cannot load.
    layerClient: NodeHttpClient.layerNodeHttpOptions({ lookup }),
  };
};
