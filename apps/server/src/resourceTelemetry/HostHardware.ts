// @effect-diagnostics-next-line nodeBuiltinImport:off -- Effect has no volume size query.
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import type { HostHardwareSnapshot, HostHardwareVolumeUse } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";

import * as ServerConfig from "../config.ts";
import { detectServerEnvironmentMachineModel } from "../environment/ServerEnvironmentMachine.ts";
import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as ProcessRunner from "../processRunner.ts";
import * as ServerSettings from "../serverSettings.ts";
import { resolveWorktreesDirectory } from "../worktreesDirectory.ts";
import * as HostResources from "./HostResources.ts";

export class HostHardware extends Context.Service<
  HostHardware,
  { readonly read: Effect.Effect<HostHardwareSnapshot> }
>()("onicode/resourceTelemetry/HostHardware") {}

type Cpu = HostHardwareSnapshot["cpu"];

// "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz" reads better as "Intel Core i7-9750H".
function cpuModelName(model: string | undefined): string | null {
  const name = (model ?? "")
    .replaceAll(/\((R|TM|C)\)/gi, "")
    .replace(/\s+@\s.*$/, "")
    .replace(/\s+(\d+-Core\s+)?Processor$/i, "")
    .replace(/\s+CPU\b/i, "")
    .replaceAll(/\s+/g, " ")
    .trim();
  return name === "" || name.toLowerCase() === "unknown" ? null : name;
}

// `sysctl name ...` prints "name: value" for each oid it knows; Intel Macs have no perflevels.
function darwinCores(sysctl: string): Pick<Cpu, "physicalCores" | "coreGroups"> {
  const value = (name: string) =>
    new RegExp(`^${name.replaceAll(".", "\\.")}: (.+)$`, "m").exec(sysctl)?.[1]?.trim();
  const count = (name: string) => {
    const cores = Number(value(name));
    return Number.isInteger(cores) && cores > 0 ? cores : null;
  };
  const coreGroups = [0, 1, 2].flatMap((level) => {
    const name = value(`hw.perflevel${level}.name`);
    const cores = count(`hw.perflevel${level}.physicalcpu`);
    return name && cores !== null ? [{ name, cores }] : [];
  });
  return {
    physicalCores: count("hw.physicalcpu"),
    coreGroups: coreGroups.length > 1 ? coreGroups : [],
  };
}

// A core is one "physical id" and "core id" pair; ARM kernels print neither.
function linuxPhysicalCores(cpuinfo: string): number | null {
  const cores = new Set<string>();
  for (const processor of cpuinfo.split(/\n\s*\n/)) {
    const core = /^core id\s*:\s*(\d+)$/m.exec(processor)?.[1];
    if (core === undefined) continue;
    cores.add(`${/^physical id\s*:\s*(\d+)$/m.exec(processor)?.[1] ?? 0}:${core}`);
  }
  return cores.size > 0 ? cores.size : null;
}

const make = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const platform = yield* HostProcess.Platform;
  const home = yield* HostProcess.HomeDirectory;
  const config = yield* ServerConfig.ServerConfig;
  const settings = yield* ServerSettings.ServerSettingsService;
  const projects = yield* ProjectStore.ProjectStoreV2;
  const processRunner = yield* ProcessRunner.ProcessRunner;
  const resources = yield* HostResources.HostResources;

  const readFile = (file: string) => fs.readFileString(file).pipe(Effect.orElseSucceed(() => ""));

  const readOs = Effect.fn("HostHardware.readOs")(function* () {
    if (platform === "darwin") {
      const plist = yield* readFile("/System/Library/CoreServices/SystemVersion.plist");
      const version = /<key>ProductVersion<\/key>\s*<string>([^<]+)<\/string>/.exec(plist)?.[1];
      return version ? `macOS ${version}` : "macOS";
    }
    if (platform === "linux") {
      const release = yield* readFile("/etc/os-release");
      const name = /^PRETTY_NAME=(.*)$/m
        .exec(release)?.[1]
        ?.replaceAll(/^["']|["']$/g, "")
        .trim();
      return name || `Linux ${NodeOS.release()}`;
    }
    // On Windows os.version() is the edition, such as "Windows 11 Pro".
    return platform === "win32" ? NodeOS.version() : `${NodeOS.type()} ${NodeOS.release()}`;
  });

  const readCores = Effect.fn("HostHardware.readCores")(function* () {
    if (platform === "darwin") {
      // Exits non-zero when any oid is unknown, yet still prints the others.
      const sysctl = yield* processRunner
        .run({
          command: "sysctl",
          args: [
            "hw.physicalcpu",
            ...[0, 1, 2].flatMap((level) => [
              `hw.perflevel${level}.name`,
              `hw.perflevel${level}.physicalcpu`,
            ]),
          ],
          timeout: "5 seconds",
          timeoutBehavior: "timedOutResult",
        })
        .pipe(
          Effect.map((result) => result.stdout),
          Effect.orElseSucceed(() => ""),
        );
      return darwinCores(sysctl);
    }
    return {
      physicalCores:
        platform === "linux" ? linuxPhysicalCores(yield* readFile("/proc/cpuinfo")) : null,
      coreGroups: [],
    };
  });

  // Nothing here changes while the machine is up, and the probes spawn processes.
  const readMachine = yield* Effect.cached(
    Effect.gen(function* () {
      const [model, os, cores] = yield* Effect.all(
        [
          detectServerEnvironmentMachineModel().pipe(
            Effect.provideService(ProcessRunner.ProcessRunner, processRunner),
            Effect.provideService(FileSystem.FileSystem, fs),
            Effect.provideService(HostProcess.Platform, platform),
          ),
          readOs(),
          readCores(),
        ],
        { concurrency: "unbounded" },
      );
      const cpus = NodeOS.cpus();
      return {
        hostname: NodeOS.hostname().trim() || null,
        model,
        os,
        cpu: {
          model: cpuModelName(cpus[0]?.model),
          logicalCores: cpus.length || NodeOS.availableParallelism(),
          ...cores,
        },
        memoryBytes: NodeOS.totalmem(),
      };
    }),
  );

  // A stalled network mount must not hold the answer; its volume is left out instead.
  const within = <A, E>(effect: Effect.Effect<A, E>) =>
    effect.pipe(
      Effect.timeout("1 second"),
      Effect.orElseSucceed(() => null),
    );
  const deviceOf = (directory: string) =>
    within(Effect.map(fs.stat(directory), (info) => info.dev));

  // statfs names no mount point: it is the highest ancestor still on the same device.
  const mountPointOf = Effect.fn("HostHardware.mountPointOf")(function* (
    directory: string,
    device: bigint,
  ) {
    let mountPoint = yield* fs.realPath(directory).pipe(Effect.orElseSucceed(() => directory));
    while (true) {
      const parent = path.dirname(mountPoint);
      if (parent === mountPoint || (yield* deviceOf(parent)) !== device) return mountPoint;
      mountPoint = parent;
    }
  });

  const readVolumes = Effect.fn("HostHardware.readVolumes")(function* () {
    const worktrees = yield* settings.getSettings.pipe(
      Effect.map((current) =>
        resolveWorktreesDirectory(current.worktreesDirectory, config.worktreesDir, path, home),
      ),
      Effect.orElseSucceed(() => null),
    );
    const projectRoots = yield* projects.listShells().pipe(
      Effect.map((shells) => shells.map((project) => project.workspaceRoot)),
      Effect.orElseSucceed((): ReadonlyArray<string> => []),
    );
    const directories: ReadonlyArray<{ use: HostHardwareVolumeUse; directory: string }> = [
      { use: "home", directory: home },
      { use: "data", directory: config.stateDir },
      { use: "worktrees", directory: worktrees ?? config.worktreesDir },
      ...[...new Set(projectRoots)].map((directory) => ({ use: "projects" as const, directory })),
    ];
    const located = yield* Effect.forEach(
      directories,
      (entry) => Effect.map(deviceOf(entry.directory), (device) => ({ ...entry, device })),
      { concurrency: 8 },
    );
    const byDevice = new Map<bigint, { directory: string; uses: Set<HostHardwareVolumeUse> }>();
    for (const { device, directory, use } of located) {
      if (device === null) continue;
      const volume = byDevice.get(device) ?? { directory, uses: new Set() };
      volume.uses.add(use);
      byDevice.set(device, volume);
    }
    const volumes = yield* Effect.forEach(
      byDevice,
      Effect.fnUntraced(function* ([device, { directory, uses }]) {
        const stats = yield* within(
          Effect.promise(() => NodeFSP.statfs(directory).catch(() => null)),
        );
        const totalBytes = stats === null ? 0 : stats.blocks * stats.bsize;
        if (stats === null || !Number.isSafeInteger(totalBytes) || totalBytes <= 0) return null;
        return {
          mountPoint: yield* mountPointOf(directory, device),
          totalBytes,
          availableBytes: Math.min(totalBytes, Math.max(0, stats.bavail * stats.bsize)),
          uses: [...uses],
        };
      }),
      { concurrency: 8 },
    );
    return volumes.filter((volume) => volume !== null);
  });
  const volumes = yield* Effect.cachedWithTTL(readVolumes(), Duration.seconds(10));

  return HostHardware.of({
    read: Effect.all([readMachine, volumes, resources.read], { concurrency: "unbounded" }).pipe(
      Effect.map(([machine, currentVolumes, usage]) => ({
        ...machine,
        uptimeSeconds: Math.max(0, Math.floor(NodeOS.uptime())),
        volumes: currentVolumes,
        usage,
      })),
      Effect.withSpan("HostHardware.read"),
    ),
  });
});

export const layer = Layer.effect(HostHardware, make);
