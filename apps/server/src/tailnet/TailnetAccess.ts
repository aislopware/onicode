import {
  type AuthAccessTokenResult,
  type AuthClientMetadata,
  type AuthEnvironmentScope,
  AuthFilesystemReadScope,
  AuthOrchestrationReadScope,
  AuthStandardClientScopes,
  AuthTerminalReadScope,
  ExecutionEnvironmentDescriptor,
  TAILNET_APP_CAPABILITY,
  type TailnetDevice,
  type TailnetDevicesResult,
} from "@onicode/contracts";
import {
  isTailscaleIpv4Address,
  readTailnetStatus,
  readTailscaleWhois,
  type TailnetNode,
  type TailnetWhois,
} from "@onicode/tailscale";
import * as Context from "effect/Context";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import { HttpClient, HttpClientRequest, HttpClientResponse } from "effect/http";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";

import * as EnvironmentAuth from "../auth/EnvironmentAuth.ts";
import * as ServerConfig from "../config.ts";
import * as ManagedLimits from "../policy/managedLimits.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import { ServerSettingsService } from "../serverSettings.ts";

/** Where Onicode listens unless told otherwise, on desktop and as a service. */
const ONICODE_PORT = ServerConfig.DEFAULT_PORT;
// Peers that drop packets would otherwise hold the list open for the OS connect timeout.
const PROBE_TIMEOUT = Duration.millis(1_500);
const PROBE_CONCURRENCY = 16;

const TAILNET_IPV6_PREFIX = /^fd7a:115c:a1e0:/iu;
const isTailnetAddress = (address: string) =>
  isTailscaleIpv4Address(address) || TAILNET_IPV6_PREFIX.test(address);

const READ_SCOPES: ReadonlyArray<AuthEnvironmentScope> = [
  AuthOrchestrationReadScope,
  AuthTerminalReadScope,
  AuthFilesystemReadScope,
];

export const TailnetSignInRefusal = Schema.Literals([
  "disabled",
  "managed-policy",
  "not-tailnet",
  "unidentified",
  "not-allowed",
]);
export type TailnetSignInRefusal = typeof TailnetSignInRefusal.Type;

export class TailnetSignInRefusedError extends Schema.TaggedError<TailnetSignInRefusedError>()(
  "TailnetSignInRefusedError",
  { reason: TailnetSignInRefusal },
) {
  override get message(): string {
    return `Tailnet sign-in refused: ${this.reason}.`;
  }
}

/**
 * The scopes a tailnet caller gets, or null. Devices of the user who owns this machine get a
 * standard session, as Tailscale's own web client allows its owner. A grant of
 * TAILNET_APP_CAPABILITY admits anyone it names; `{ "access": "read" }` values alone mean read only.
 */
export const tailnetSignInScopes = (input: {
  readonly self: TailnetNode | null;
  readonly caller: TailnetWhois;
}): ReadonlyArray<AuthEnvironmentScope> | null => {
  const granted = input.caller.capabilities[TAILNET_APP_CAPABILITY];
  if (granted !== undefined && granted.length > 0) {
    const readOnly = granted.every(
      (value) =>
        typeof value === "object" &&
        value !== null &&
        (value as { readonly access?: unknown }).access === "read",
    );
    return readOnly ? READ_SCOPES : AuthStandardClientScopes;
  }
  const self = input.self;
  if (
    self !== null &&
    self.owner !== null &&
    self.userId !== null &&
    input.caller.user !== null &&
    input.caller.user.id === self.userId
  ) {
    return AuthStandardClientScopes;
  }
  return null;
};

/** The direct TCP peer of a request, read from its socket. */
export interface TailnetPeer {
  readonly remoteAddress: string | undefined;
  readonly remotePort: number | undefined;
  readonly localAddress: string | undefined;
}

const stripMappedIpv4 = (address: string | undefined) => address?.replace(/^::ffff:/iu, "");

export class TailnetAccess extends Context.Service<
  TailnetAccess,
  {
    readonly listDevices: () => Effect.Effect<TailnetDevicesResult>;
    readonly signInEnabled: () => Effect.Effect<boolean>;
    readonly signIn: (input: {
      readonly peer: TailnetPeer;
      readonly client: AuthClientMetadata;
    }) => Effect.Effect<
      AuthAccessTokenResult,
      TailnetSignInRefusedError | EnvironmentAuth.ServerAuthInternalError
    >;
  }
>()("onicode/tailnet/TailnetAccess") {}

const decodeDescriptor = Schema.decodeUnknownEffect(ExecutionEnvironmentDescriptor);

const make = Effect.gen(function* () {
  const config = yield* ServerConfig.ServerConfig;
  const settings = yield* ServerSettingsService;
  const managedPolicy = yield* ManagedPolicy.ManagedPolicy;
  const auth = yield* EnvironmentAuth.EnvironmentAuth;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const httpClient = yield* HttpClient.HttpClient;

  const withCli = Effect.provideService(ChildProcessSpawner.ChildProcessSpawner, spawner);

  const probeOnicode = Effect.fn("TailnetAccess.probeOnicode")(
    function* (address: string, port: number) {
      const httpBaseUrl = `http://${address}:${port}`;
      const response = yield* httpClient.execute(
        HttpClientRequest.get(`${httpBaseUrl}/.well-known/onicode/environment`),
      );
      const descriptor = yield* HttpClientResponse.filterStatusOk(response).pipe(
        Effect.flatMap((ok) => ok.json),
        Effect.flatMap(decodeDescriptor),
      );
      return {
        environmentId: descriptor.environmentId,
        label: descriptor.label,
        serverVersion: descriptor.serverVersion,
        httpBaseUrl,
        tailnetSignIn: descriptor.capabilities.tailnetSignIn === true,
      };
    },
    Effect.timeoutOption(PROBE_TIMEOUT),
    Effect.map((found) => (found._tag === "Some" ? found.value : undefined)),
    Effect.orElseSucceed(() => undefined),
  );

  const toDevice = Effect.fnUntraced(function* (node: TailnetNode, self: boolean) {
    const ipv4 = node.addresses.find(isTailscaleIpv4Address);
    const onicode =
      node.online && ipv4 !== undefined
        ? yield* probeOnicode(ipv4, self ? config.port : ONICODE_PORT)
        : undefined;
    return {
      id: node.id,
      name: node.name,
      ...(node.dnsName === undefined ? {} : { dnsName: node.dnsName }),
      addresses: node.addresses,
      ...(node.os === undefined ? {} : { os: node.os }),
      online: node.online,
      self,
      owner: node.owner,
      tags: node.tags,
      ...(onicode === undefined ? {} : { onicode }),
    } satisfies TailnetDevice;
  });

  const listDevices = Effect.fn("TailnetAccess.listDevices")(function* () {
    const status = yield* readTailnetStatus.pipe(withCli, Effect.option);
    if (status._tag === "None" || !status.value.running) {
      return { available: false, devices: [] } satisfies TailnetDevicesResult;
    }
    const { self, peers, tailnetName } = status.value;
    const nodes = [
      ...(self === null ? [] : [{ node: self, self: true }]),
      ...peers.map((node) => ({ node, self: false })),
    ];
    const devices = yield* Effect.forEach(nodes, ({ node, self }) => toDevice(node, self), {
      concurrency: PROBE_CONCURRENCY,
    });
    return {
      available: true,
      ...(tailnetName === undefined ? {} : { tailnetName }),
      devices,
    } satisfies TailnetDevicesResult;
  });

  const signInEnabled = Effect.fn("TailnetAccess.signInEnabled")(function* () {
    if (!ManagedLimits.allowsRemoteClients(managedPolicy)) return false;
    const current = yield* settings.getSettings.pipe(Effect.option);
    return current._tag === "Some" && current.value.tailnetSignIn;
  });

  const refuse = (reason: TailnetSignInRefusal) => new TailnetSignInRefusedError({ reason });

  const signIn: TailnetAccess["Service"]["signIn"] = Effect.fn("TailnetAccess.signIn")(function* ({
    peer,
    client,
  }) {
    if (!ManagedLimits.allowsRemoteClients(managedPolicy)) {
      return yield* refuse("managed-policy");
    }
    if (!(yield* signInEnabled())) return yield* refuse("disabled");
    const remoteAddress = stripMappedIpv4(peer.remoteAddress);
    const localAddress = stripMappedIpv4(peer.localAddress);
    // Both ends on the tailnet: the packet came through tailscaled, which allows no spoofed
    // source address. A userspace tailscaled delivers from loopback, which is refused here.
    if (
      remoteAddress === undefined ||
      localAddress === undefined ||
      peer.remotePort === undefined ||
      !isTailnetAddress(remoteAddress) ||
      !isTailnetAddress(localAddress)
    ) {
      return yield* refuse("not-tailnet");
    }
    const target = remoteAddress.includes(":")
      ? `[${remoteAddress}]:${peer.remotePort}`
      : `${remoteAddress}:${peer.remotePort}`;
    const [caller, status] = yield* Effect.all([readTailscaleWhois(target), readTailnetStatus], {
      concurrency: "unbounded",
    }).pipe(
      withCli,
      Effect.mapError(() => refuse("unidentified")),
    );
    const scopes = tailnetSignInScopes({ self: status.self, caller });
    if (scopes === null) {
      yield* Effect.logInfo("Tailnet sign-in refused a device its policy does not allow.", {
        tagged: caller.tags.length > 0,
      });
      return yield* refuse("not-allowed");
    }
    return yield* auth.issueAccessToken({
      subject: `${EnvironmentAuth.TAILNET_SESSION_SUBJECT_PREFIX}${caller.nodeId}`,
      scopes,
      client: {
        ...client,
        label: caller.name,
        ...(client.os === undefined && caller.os !== undefined ? { os: caller.os } : {}),
      },
    });
  });

  return TailnetAccess.of({ listDevices, signInEnabled, signIn });
});

export const layer = Layer.effect(TailnetAccess, make);

/** Tailscale off on this machine: no devices, and every sign-in refused. */
export const layerUnavailable = Layer.succeed(
  TailnetAccess,
  TailnetAccess.of({
    listDevices: Effect.fn("TailnetAccess.listDevices")(() =>
      Effect.succeed({ available: false, devices: [] }),
    ),
    signInEnabled: Effect.fn("TailnetAccess.signInEnabled")(() => Effect.succeed(false)),
    signIn: Effect.fn("TailnetAccess.signIn")(() =>
      Effect.fail(new TailnetSignInRefusedError({ reason: "disabled" })),
    ),
  }),
);
