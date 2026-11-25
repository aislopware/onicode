import type { ServerDirectEndpoint } from "@onicode/contracts";
import {
  buildTailscaleHttpsBaseUrl,
  probeTailscaleHttpsEndpoint,
  readTailscaleStatus,
} from "@onicode/tailscale";
import * as Context from "effect/Context";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import { HttpClient } from "effect/http";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import * as NodeNet from "node:net";
import * as NodeOS from "node:os";

import * as HostProcess from "@onicode/shared/HostProcess";
import {
  formatHostForUrl,
  isMachineLocalInterface,
  isTailnetHost,
  isWildcardHost,
} from "@onicode/shared/hostClassification";

import * as ServerConfig from "../config.ts";
import * as ServerEnvironment from "./ServerEnvironment.ts";
import { isLoopbackHost, isPrivateNetworkAddress } from "../startupAccess.ts";

type NetworkInterfacesMap = ReturnType<typeof NodeOS.networkInterfaces>;

// Stay below the config discovery timeout so slow Tailscale drops only the tailnet name.
const TAILSCALE_ENDPOINT_TIMEOUT = Duration.seconds(3);

export class DirectEndpoints extends Context.Service<
  DirectEndpoints,
  {
    readonly resolveAvailable: () => Effect.Effect<ReadonlyArray<ServerDirectEndpoint>>;
    readonly resolve: () => Effect.Effect<ReadonlyArray<ServerDirectEndpoint>>;
  }
>()("onicode/environment/DirectEndpoints") {}

// Credentials travel over HTTP; public IPs leak them and hostnames can resolve differently per client.
// Wildcard binds stay IPv4: the private-network listener check accepts only IPv4 peers.
const isAdvertisableAddress = (address: string): boolean =>
  NodeNet.isIPv4(address) && !address.startsWith("127.") && isPrivateNetworkAddress(address);

const isAdvertisableWildcardEntry = (
  name: string,
  entry: NodeOS.NetworkInterfaceInfo,
  platform: NodeJS.Platform,
): boolean =>
  !entry.internal &&
  entry.family === "IPv4" &&
  isAdvertisableAddress(entry.address) &&
  !isMachineLocalInterface(name, entry.mac, platform);

// macOS utun names need Tailscale's IPv6 range to identify them; WARP shares its IPv4 range.
const isTailscaleInterface = (
  name: string,
  entries: ReadonlyArray<NodeOS.NetworkInterfaceInfo>,
): boolean =>
  /^tailscale/i.test(name) ||
  entries.some((entry) => entry.family === "IPv6" && isTailnetHost(entry.address));

export function resolveBoundEndpoints(input: {
  readonly platform: NodeJS.Platform;
  readonly host: string | undefined;
  readonly port: number;
  readonly interfaces: NetworkInterfacesMap;
  /** A cloud VM's private network is its provider's, which no client shares. */
  readonly onCloudVm?: boolean;
  /** The listener refuses LAN connections, so only tailnet addresses work. */
  readonly tailnetOnly?: boolean;
  readonly isolatedBridges?: ReadonlySet<string>;
}): ReadonlyArray<ServerDirectEndpoint> {
  if (isLoopbackHost(input.host)) return [];
  const tailscaleAddresses = new Set(
    Object.entries(input.interfaces).flatMap(([name, entries = []]) =>
      isTailscaleInterface(name, entries) ? entries.map((entry) => entry.address) : [],
    ),
  );
  const addresses = isWildcardHost(input.host)
    ? Object.entries(input.interfaces)
        .flatMap(([name, entries]) =>
          input.isolatedBridges?.has(name)
            ? []
            : (entries ?? []).filter((entry) =>
                isAdvertisableWildcardEntry(name, entry, input.platform),
              ),
        )
        .map((entry) => entry.address)
    : [input.host!].filter(
        (address) =>
          isAdvertisableAddress(address) || (NodeNet.isIPv6(address) && isTailnetHost(address)),
      );
  return [...new Set(addresses)]
    .filter((address) => !(input.onCloudVm || input.tailnetOnly) || tailscaleAddresses.has(address))
    .map((address) => ({
      kind: tailscaleAddresses.has(address) ? "tailnet" : "lan",
      httpBaseUrl: `http://${formatHostForUrl(address)}:${input.port}/`,
    }));
}

const SYSFS_NET = "/sys/class/net";

/**
 * Linux bridges with no port leading to a NIC, such as a container network
 * under any name. A host bridge like `br0` enslaves the NIC, directly or
 * through a bond or VLAN, and stays listed.
 */
export const readIsolatedLinuxBridges = Effect.fn("DirectEndpoints.readIsolatedLinuxBridges")(
  function* (names: ReadonlyArray<string>, root = SYSFS_NET) {
    const fs = yield* FileSystem.FileSystem;
    const exists = (path: string) => fs.exists(path).pipe(Effect.orElseSucceed(() => false));
    const deviceExists = (path: string) =>
      fs.exists(path).pipe(
        Effect.catchIf(
          (error) => error.reason._tag === "NotFound",
          () => Effect.succeed(false),
        ),
        Effect.orElseSucceed(() => true),
      );
    const readDirOrEmptyOnMissing = (path: string) =>
      fs.readDirectory(path).pipe(
        Effect.catchIf(
          (error) => error.reason._tag === "NotFound",
          () => Effect.succeed<ReadonlyArray<string>>([]),
        ),
        Effect.option,
      );
    const reachesHardware: (name: string, depth: number) => Effect.Effect<boolean> =
      Effect.fnUntraced(function* (name: string, depth: number) {
        if (yield* deviceExists(`${root}/${name}/device`)) return true;
        if (depth === 0) return false;
        const entries = yield* readDirOrEmptyOnMissing(`${root}/${name}`);
        if (Option.isNone(entries)) return true;
        const lowers = entries.value.flatMap((entry) =>
          entry.startsWith("lower_") ? [entry.slice("lower_".length)] : [],
        );
        for (const lower of lowers) {
          if (yield* reachesHardware(lower, depth - 1)) return true;
        }
        return false;
      });
    const isBaseBridgeIsolated = Effect.fnUntraced(function* (base: string) {
      if (!(yield* exists(`${root}/${base}/bridge`))) return false;
      const ports = yield* readDirOrEmptyOnMissing(`${root}/${base}/brif`);
      // Unreadable brif (e.g. EACCES) cannot prove the bridge has no uplink.
      if (Option.isNone(ports)) return false;
      for (const port of ports.value) {
        if (yield* reachesHardware(port, 3)) return false;
      }
      return true;
    });
    const baseIsolated = new Map<string, boolean>();
    const isolated = new Set<string>();
    for (const name of names) {
      // Labeled IPv4 aliases like `br0:0` share the base interface's sysfs entry.
      const base = name.split(":", 1)[0] ?? "";
      if (base === "") continue;
      let isIsolated = baseIsolated.get(base);
      if (isIsolated === undefined) {
        isIsolated = yield* isBaseBridgeIsolated(base);
        baseIsolated.set(base, isIsolated);
      }
      if (isIsolated) isolated.add(name);
    }
    return isolated;
  },
);

/**
 * AWS, GCP, Azure, DigitalOcean, Hetzner, Linode, Vultr, and Oracle all serve
 * instance metadata at this address. A plain TCP connect skips HTTP proxies,
 * which would otherwise answer for a machine that is not in a cloud.
 */
const CLOUD_METADATA_ADDRESS = "169.254.169.254";
const CLOUD_METADATA_TIMEOUT_MS = 300;

const probeCloudMetadata = Effect.callback<boolean>((resume) => {
  const socket = NodeNet.connect({
    host: CLOUD_METADATA_ADDRESS,
    port: 80,
    timeout: CLOUD_METADATA_TIMEOUT_MS,
  });
  let settled = false;
  const settle = (found: boolean) => {
    if (settled) return;
    settled = true;
    socket.destroy();
    resume(Effect.succeed(found));
  };
  socket.once("connect", () => settle(true));
  socket.once("timeout", () => settle(false));
  socket.once("error", () => settle(false));
  return Effect.sync(() => socket.destroy());
});

/** @public */
export const make = Effect.gen(function* () {
  const config = yield* ServerConfig.ServerConfig;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const httpClient = yield* HttpClient.HttpClient;
  const platform = yield* HostProcess.Platform;
  const fileSystem = yield* FileSystem.FileSystem;
  const environmentId = yield* (yield* ServerEnvironment.ServerEnvironment).getEnvironmentId;
  // Only the service default listens on every interface without being asked,
  // so only it checks once whether it runs in a cloud. The check starts now so
  // the first client does not wait for its timeout.
  const onCloudVm = config.privateNetworkOnly
    ? yield* Effect.cached(probeCloudMetadata)
    : Effect.succeed(false);
  yield* Effect.forkDetach(onCloudVm);

  const servedEndpoint = yield* Ref.make<Option.Option<string>>(Option.none());
  const resolveAvailable = Effect.fn("DirectEndpoints.resolveAvailable")(function* () {
    const interfaces = NodeOS.networkInterfaces();
    const cloudVm = yield* onCloudVm;
    const tailnetOnly = config.tailnetOnly === true;
    const bridgeCandidates =
      platform === "linux" && isWildcardHost(config.host) && !cloudVm && !tailnetOnly
        ? Object.entries(interfaces).flatMap(([name, entries = []]) =>
            !isTailscaleInterface(name, entries) &&
            entries.some((entry) => isAdvertisableWildcardEntry(name, entry, platform))
              ? [name]
              : [],
          )
        : [];
    const isolatedBridges =
      bridgeCandidates.length > 0
        ? yield* readIsolatedLinuxBridges(bridgeCandidates).pipe(
            Effect.provideService(FileSystem.FileSystem, fileSystem),
          )
        : undefined;
    const endpoints = [
      ...resolveBoundEndpoints({
        platform,
        host: config.host,
        port: config.port,
        interfaces,
        onCloudVm: cloudVm,
        tailnetOnly,
        ...(isolatedBridges === undefined ? {} : { isolatedBridges }),
      }),
    ];
    const servedUrl = yield* Ref.get(servedEndpoint);
    if (Option.isSome(servedUrl)) endpoints.push({ kind: "tailnet", httpBaseUrl: servedUrl.value });
    return endpoints;
  });

  const resolve = Effect.gen(function* () {
    // Serve setup can fail without stopping startup; advertise only after it answers as this server.
    if (config.tailscaleServeEnabled) {
      const servedUrl = yield* readTailscaleStatus.pipe(
        Effect.map((status) => status.magicDnsName),
        Effect.flatMap((magicDnsName) => {
          if (magicDnsName === null) return Effect.succeed(null);
          const httpBaseUrl = buildTailscaleHttpsBaseUrl({
            magicDnsName,
            servePort: config.tailscaleServePort,
          });
          return probeTailscaleHttpsEndpoint({ baseUrl: httpBaseUrl, environmentId }).pipe(
            Effect.map((reachable) => (reachable ? httpBaseUrl : null)),
          );
        }),
        Effect.timeoutOption(TAILSCALE_ENDPOINT_TIMEOUT),
        Effect.map(Option.flatMap(Option.fromNullishOr)),
        Effect.orElseSucceed(() => Option.none<string>()),
        Effect.provideService(ChildProcessSpawner.ChildProcessSpawner, spawner),
        Effect.provideService(HttpClient.HttpClient, httpClient),
      );
      yield* Ref.set(servedEndpoint, servedUrl);
    }
    return yield* resolveAvailable();
  });

  return DirectEndpoints.of({
    resolveAvailable,
    resolve: Effect.fn("DirectEndpoints.resolve")(function () {
      return resolve;
    }),
  });
});

export const layer = Layer.effect(DirectEndpoints, make);
