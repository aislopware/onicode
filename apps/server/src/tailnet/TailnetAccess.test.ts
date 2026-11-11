import { assert, describe, it } from "@effect/vitest";
import {
  AuthFilesystemReadScope,
  AuthOrchestrationReadScope,
  AuthStandardClientScopes,
  AuthTerminalReadScope,
  TAILNET_APP_CAPABILITY,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import type { TailnetNode, TailnetWhois } from "@onicode/tailscale";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { HttpClient } from "effect/http";
import { ChildProcessSpawner } from "effect/process";

import * as NodeServices from "@effect/platform-node/NodeServices";

import * as EnvironmentAuth from "../auth/EnvironmentAuth.ts";
import * as ServerConfig from "../config.ts";
import type { ManagedPolicyDocument } from "@onicode/contracts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import { ServerSettingsService } from "../serverSettings.ts";
import * as TailnetAccess from "./TailnetAccess.ts";

const owner = { loginName: "alice@example.dev", displayName: "Alice" };
const self: TailnetNode = {
  id: "nSelf",
  name: "alice-mac-mini",
  addresses: ["100.64.0.67"],
  online: true,
  userId: 2,
  tags: [],
  owner,
};
const caller = (overrides: Partial<TailnetWhois> = {}): TailnetWhois => ({
  nodeId: "nLaptop",
  name: "laptop",
  tags: [],
  user: { id: 2, ...owner },
  capabilities: {},
  ...overrides,
});
const READ_SCOPES = [AuthOrchestrationReadScope, AuthTerminalReadScope, AuthFilesystemReadScope];

describe("tailnetSignInScopes", () => {
  it.each([
    ["the machine owner's other device", self, caller(), AuthStandardClientScopes],
    [
      "another user",
      self,
      caller({ user: { id: 4, loginName: "bob@example.dev", displayName: "Bob" } }),
      null,
    ],
    ["a tagged device", self, caller({ user: null, tags: ["tag:ci"] }), null],
    // A tagged server belongs to no one, so only a grant admits anybody.
    ["anyone, to a tagged server", { ...self, owner: null, tags: ["tag:host"] }, caller(), null],
    [
      "a grant, to a tagged server",
      { ...self, owner: null, tags: ["tag:host"] },
      caller({ user: null, tags: ["tag:ci"], capabilities: { [TAILNET_APP_CAPABILITY]: [{}] } }),
      AuthStandardClientScopes,
    ],
    [
      "a read grant to another user",
      self,
      caller({
        user: { id: 4, loginName: "bob@example.dev", displayName: "Bob" },
        capabilities: { [TAILNET_APP_CAPABILITY]: [{ access: "read" }] },
      }),
      READ_SCOPES,
    ],
    [
      "a read grant to the owner, which narrows the owner too",
      self,
      caller({ capabilities: { [TAILNET_APP_CAPABILITY]: [{ access: "read" }] } }),
      READ_SCOPES,
    ],
    [
      "read and full grants together",
      self,
      caller({ capabilities: { [TAILNET_APP_CAPABILITY]: [{ access: "read" }, {}] } }),
      AuthStandardClientScopes,
    ],
  ] as const)("admits %s as expected", (_name, server, who, expected) => {
    assert.deepEqual(tailnetSignInScopes(server, who), expected === null ? null : [...expected]);
  });
});

function tailnetSignInScopes(server: TailnetNode, who: TailnetWhois): Array<string> | null {
  const scopes = TailnetAccess.tailnetSignInScopes({ self: server, caller: who });
  return scopes === null ? null : [...scopes];
}

const encoder = new TextEncoder();
const STATUS_JSON = JSON.stringify({
  BackendState: "Running",
  MagicDNSSuffix: "ts.example.dev",
  Self: {
    ID: "nSelf",
    DNSName: "alice-mac-mini.ts.example.dev.",
    TailscaleIPs: ["100.64.0.67"],
    Online: true,
    UserID: 2,
  },
  User: { "2": { LoginName: owner.loginName, DisplayName: owner.displayName } },
});
const whoisJson = (userId: number) =>
  JSON.stringify({
    Node: { StableID: "nLaptop", ComputedName: "laptop", Hostinfo: { OS: "macOS" } },
    UserProfile: { ID: userId, LoginName: `user${userId}@example.dev`, DisplayName: "Someone" },
  });

const layerAccess = (input: {
  readonly tailnetSignIn: boolean;
  readonly whoisUserId?: number;
  readonly policy?: ManagedPolicyDocument;
  readonly whoisArgs?: Array<ReadonlyArray<string>>;
  readonly issued?: Array<
    Parameters<EnvironmentAuth.EnvironmentAuth["Service"]["issueAccessToken"]>[0]
  >;
}) =>
  TailnetAccess.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        ServerConfig.layerTest(process.cwd(), { prefix: "oni-tailnet-access-test-" }).pipe(
          Layer.provide(NodeServices.layer),
        ),
        ServerSettingsService.layerTest({ tailnetSignIn: input.tailnetSignIn }),
        ManagedPolicy.layerTest(input.policy),
        Layer.succeed(
          HttpClient.HttpClient,
          HttpClient.make(() => Effect.die("sign-in probes nothing")),
        ),
        Layer.mock(EnvironmentAuth.EnvironmentAuth)({
          issueAccessToken: (request) => {
            input.issued?.push(request);
            return Effect.succeed({
              access_token: "token",
              issued_token_type: "urn:ietf:params:oauth:token-type:access_token",
              token_type: "Bearer",
              expires_in: 60,
              scope: "orchestration:read",
            });
          },
        }),
        Layer.succeed(
          ChildProcessSpawner.ChildProcessSpawner,
          ChildProcessSpawner.make((command) => {
            const { args } = command as unknown as { readonly args: ReadonlyArray<string> };
            if (args[0] === "whois") input.whoisArgs?.push(args);
            const stdout = args[0] === "whois" ? whoisJson(input.whoisUserId ?? 2) : STATUS_JSON;
            return Effect.succeed(
              ChildProcessSpawner.makeHandle({
                pid: ChildProcessSpawner.ProcessId(1),
                exitCode: Effect.succeed(ChildProcessSpawner.ExitCode(0)),
                isRunning: Effect.succeed(false),
                kill: () => Effect.void,
                unref: Effect.succeed(Effect.void),
                stdin: Sink.drain,
                stdout: Stream.make(encoder.encode(stdout)),
                stderr: Stream.empty,
                all: Stream.empty,
                getInputFd: () => Sink.drain,
                getOutputFd: () => Stream.empty,
              }),
            );
          }),
        ),
        Layer.succeed(HostProcess.Platform, "linux"),
      ),
    ),
  );

const tailnetPeer = {
  remoteAddress: "::ffff:100.64.0.2",
  remotePort: 51234,
  localAddress: "::ffff:100.64.0.67",
};

const signIn = (peer: TailnetAccess.TailnetPeer) =>
  TailnetAccess.TailnetAccess.pipe(
    Effect.flatMap((access) => access.signIn({ peer, client: { deviceType: "desktop" } })),
  );

const refusal = (peer: TailnetAccess.TailnetPeer) =>
  signIn(peer).pipe(
    Effect.flip,
    Effect.map((error) => (error._tag === "TailnetSignInRefusedError" ? error.reason : error._tag)),
  );

describe("TailnetAccess.signIn", () => {
  it.effect("signs in the owner's device by the whois of its exact address and port", () => {
    const whoisArgs: Array<ReadonlyArray<string>> = [];
    const issued: Array<
      Parameters<EnvironmentAuth.EnvironmentAuth["Service"]["issueAccessToken"]>[0]
    > = [];
    return Effect.gen(function* () {
      yield* signIn(tailnetPeer);
      assert.deepEqual(whoisArgs, [["whois", "--json", "100.64.0.2:51234"]]);
      assert.deepEqual(
        issued.map(({ subject, client }) => ({ subject, client })),
        [
          {
            subject: "tailnet:nLaptop",
            client: { deviceType: "desktop", label: "laptop", os: "macOS" },
          },
        ],
      );
    }).pipe(Effect.provide(layerAccess({ tailnetSignIn: true, whoisArgs, issued })));
  });

  it.effect("refuses another user's device", () =>
    Effect.gen(function* () {
      assert.equal(yield* refusal(tailnetPeer), "not-allowed");
    }).pipe(Effect.provide(layerAccess({ tailnetSignIn: true, whoisUserId: 4 }))),
  );

  it.effect("refuses addresses off the tailnet, including a userspace tailscaled's loopback", () =>
    Effect.gen(function* () {
      for (const peer of [
        { ...tailnetPeer, remoteAddress: "192.168.1.31" },
        { ...tailnetPeer, localAddress: "192.168.1.20" },
        { remoteAddress: "127.0.0.1", remotePort: 51234, localAddress: "127.0.0.1" },
      ]) {
        assert.equal(yield* refusal(peer), "not-tailnet");
      }
    }).pipe(Effect.provide(layerAccess({ tailnetSignIn: true }))),
  );

  it.effect("refuses everyone while sign-in is off or the policy keeps clients local", () =>
    Effect.gen(function* () {
      assert.equal(
        yield* refusal(tailnetPeer).pipe(Effect.provide(layerAccess({ tailnetSignIn: false }))),
        "disabled",
      );
      assert.equal(
        yield* refusal(tailnetPeer).pipe(
          Effect.provide(
            layerAccess({
              tailnetSignIn: true,
              policy: {
                schemaVersion: 1,
                organization: "Example Corp",
                limits: { clients: ["desktop", "local"] },
              },
            }),
          ),
        ),
        "managed-policy",
      );
    }),
  );
});
