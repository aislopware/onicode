import { NodeHttpServer } from "@effect/platform-node";
import { expect, it } from "@effect/vitest";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { HttpClient, HttpRouter, HttpServer, HttpServerResponse } from "effect/http";

import * as McpWebhookTransport from "./McpWebhookTransport.ts";

it.each([
  // Private/LAN targets
  ["10.1.2.3", true, true],
  ["172.16.0.1", true, true],
  ["192.168.1.1", true, true],
  ["fc00::1", true, true],
  ["fd00::1", true, true],
  // Tailscale targets
  ["100.64.0.1", true, true],
  ["100.100.100.100", true, true],
  ["100.127.255.254", true, true],
  ["fd7a:115c:a1e0::1", true, true],
  // Public targets
  ["8.8.8.8", true, false],
  ["2606:4700:4700::1111", true, false],
  // Refused targets: loopback, link-local/metadata, unspecified, reserved, IPv4-mapped
  ["127.0.0.1", false, false],
  ["::1", false, false],
  ["169.254.169.254", false, false],
  ["fe80::1", false, false],
  ["0.0.0.0", false, false],
  ["::", false, false],
  ["::ffff:127.0.0.1", false, false],
  ["fec0::1", false, false],
  ["5f00::1", false, false],
  ["::ffff:0:102:304", false, false],
  ["192.0.2.1", false, false],
  ["224.0.0.1", false, false],
  // Cloud metadata inside the private and Tailscale ranges, in every spelling
  ["100.100.100.200", false, false],
  ["::ffff:100.100.100.200", false, false],
  ["::ffff:6464:64c8", false, false],
  ["fd00:ec2::254", false, false],
  ["fd00:ec2:0:0:0:0:0:254", false, false],
])("classifies %s as allowed=%s privateOrTailnet=%s", (address, allowed, privateOrTailnet) => {
  expect(McpWebhookTransport.isAllowedAddress(address)).toBe(allowed);
  expect(McpWebhookTransport.isPrivateOrTailnetAddress(address)).toBe(privateOrTailnet);
});

it.each([
  // Private/LAN IPs and Tailscale targets allow plaintext http
  ["http://10.1.2.3/hook", true],
  ["http://172.16.0.1:8080/hook", true],
  ["http://192.168.1.1/hook", true],
  ["http://[fc00::1]/hook", true],
  ["http://100.64.0.1/hook", true],
  ["http://[fd7a:115c:a1e0::1]:8080/hook", true],
  ["http://bot.tailnet.ts.net/hook", true],
  // Allowlisted loopback allows plaintext http
  ["http://127.0.0.1/hook", true],
  // Public hosts and unlisted loopback/metadata require https (or are refused)
  ["http://example.com/hook", false],
  ["http://8.8.8.8/hook", false],
  ["http://[2606:4700:4700::1111]/hook", false],
  ["http://localhost/hook", false],
  ["http://169.254.169.254/hook", false],
  ["http://100.100.100.200/latest/meta-data", false],
  ["http://[fd00:ec2::254]/latest/meta-data", false],
])("allowsPlaintext(%s) is %s", (url, expected) => {
  const guard = McpWebhookTransport.make(["127.0.0.1"]);
  expect(guard.allowsPlaintext(new URL(url))).toBe(expected);
});

it.effect(
  "admits private/LAN, tailnet and public https callbacks while enforcing transport guards",
  () =>
    Effect.gen(function* () {
      yield* HttpRouter.serve(
        Layer.mergeAll(
          HttpRouter.add("POST", "/redirect", HttpServerResponse.redirect("/target")),
          HttpRouter.add("POST", "/target", HttpServerResponse.text("reached")),
        ),
        { disableListenLog: true, disableLogger: true },
      ).pipe(Layer.build);
      const port = new URL(HttpServer.formatAddress((yield* HttpServer.HttpServer).address)).port;
      const dns: Record<string, ReadonlyArray<string>> = {
        "bot.tailnet.ts.net": ["100.64.0.5"],
        "funnel.tailnet.ts.net": ["8.8.8.8"],
        "hooks.example.com": ["93.184.216.34"],
        localhost: ["127.0.0.1"],
      };
      const guard = McpWebhookTransport.make(["127.0.0.1"], {
        resolve: (host) => Effect.succeed(dns[host] ?? []),
      });
      const client = Context.get(yield* Layer.build(guard.layerClient), HttpClient.HttpClient);

      const refused = (url: string) =>
        guard.check(new URL(url)).pipe(
          Effect.flip,
          Effect.map((error) => error._tag),
        );

      // Private/LAN targets (http and https)
      yield* guard.check(new URL("http://10.0.0.1:8080/hook"));
      yield* guard.check(new URL("https://172.16.0.5/hook"));
      yield* guard.check(new URL("http://192.168.1.50/hook"));
      yield* guard.check(new URL("http://[fc00::1]:8080/hook"));
      // Tailscale targets (IPv4, IPv6, *.ts.net)
      yield* guard.check(new URL("http://100.64.0.1:8080/hook"));
      yield* guard.check(new URL("https://100.100.100.100/hook"));
      yield* guard.check(new URL("http://[fd7a:115c:a1e0::1]:8080/hook"));
      yield* guard.check(new URL("http://bot.tailnet.ts.net/hook"));
      yield* guard.check(new URL("https://bot.tailnet.ts.net/hook"));
      yield* guard.check(new URL("https://funnel.tailnet.ts.net/hook"));
      // Public targets require https
      yield* guard.check(new URL("https://8.8.8.8/hook"));
      yield* guard.check(new URL("https://[2606:4700:4700::1111]/hook"));
      yield* guard.check(new URL("https://hooks.example.com/hook"));
      expect(yield* refused("http://8.8.8.8/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("http://hooks.example.com/hook")).toBe("WebhookAddressRejected");
      // A *.ts.net host resolving to a public IP still requires https.
      expect(yield* refused("http://funnel.tailnet.ts.net/hook")).toBe("WebhookAddressRejected");
      // Non-http(s) schemes and URL credentials are refused.
      expect(yield* refused("ftp://192.168.1.1/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("https://user:pass@192.168.1.1/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("http://user:pass@100.64.0.1/hook")).toBe("WebhookAddressRejected");
      // Loopback (unless allowlisted), link-local/metadata and unspecified addresses are refused.
      expect(yield* refused("https://[::1]/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("https://localhost/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("https://169.254.169.254/latest/meta-data")).toBe(
        "WebhookAddressRejected",
      );
      expect(yield* refused("https://[fe80::1]/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("https://0.0.0.0/hook")).toBe("WebhookAddressRejected");
      expect(yield* refused("https://missing.invalid/hook")).toBe("WebhookAddressRejected");
      yield* guard.check(new URL(`http://127.0.0.1:${port}/hook`));

      // A redirect comes back as it is, never followed.
      const redirected = yield* client.post(`http://127.0.0.1:${port}/redirect`);
      expect(redirected.status).toBe(302);
      // The client resolves again when it connects, so a name for a blocked address fails there.
      const pinned = yield* Effect.flip(client.post(`http://localhost:${port}/target`));
      expect(pinned.reason._tag).toBe("TransportError");
      expect(String((pinned.reason as { readonly cause?: unknown }).cause)).toContain(
        "localhost is not an allowed webhook address",
      );
    }).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest)),
);
