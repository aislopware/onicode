import { assert, describe, it } from "@effect/vitest";
// @effect-diagnostics-next-line nodeBuiltinImport:off -- typing the statfs mock.
import type * as NodeFSP from "node:fs/promises";
import type * as NodeOS from "node:os";
import type { HostResourcesSnapshot } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import { vi } from "vite-plus/test";

import * as ServerConfig from "../config.ts";
import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as ProcessRunner from "../processRunner.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as HostHardware from "./HostHardware.ts";
import * as HostResources from "./HostResources.ts";

const GiB = 1024 ** 3;
const host = vi.hoisted(() => ({
  cpuModel: "",
  cpus: 0,
  volumes: {} as Record<string, { blocks: number; bavail: number }>,
}));
vi.mock("node:os", async (importOriginal) => ({
  ...(await importOriginal<typeof NodeOS>()),
  hostname: () => "build-box",
  totalmem: () => 32 * GiB,
  uptime: () => 90_061.7,
  cpus: () => Array.from({ length: host.cpus }, () => ({ model: host.cpuModel })),
}));
vi.mock("node:fs/promises", async (importOriginal) => ({
  ...(await importOriginal<typeof NodeFSP>()),
  statfs: async (directory: string) => {
    const volume = host.volumes[directory];
    if (!volume) throw new Error(`no volume at ${directory}`);
    return { ...volume, bsize: 4096 };
  },
}));

const usage: HostResourcesSnapshot = {
  sampledAt: 1,
  cpuUtilization: 0.25,
  cpuCount: 8,
  availableMemoryBytes: 8 * GiB,
  totalMemoryBytes: 32 * GiB,
};

const notFound = (method: string, path: string) =>
  PlatformError.systemError({
    _tag: "NotFound",
    module: "FileSystem",
    method,
    pathOrDescriptor: path,
  });

const machine = (input: {
  readonly platform: NodeJS.Platform;
  readonly home: string;
  readonly files?: Record<string, string>;
  /** Device of each directory that exists. */
  readonly devices?: Record<string, bigint>;
  readonly commands?: Record<string, { readonly stdout: string; readonly code: number }>;
  readonly projectRoots?: ReadonlyArray<string>;
}) =>
  HostHardware.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Path.layer,
        FileSystem.layerNoop({
          readFileString: (path) => {
            const file = input.files?.[path];
            return file === undefined
              ? Effect.fail(notFound("readFileString", path))
              : Effect.succeed(file);
          },
          realPath: Effect.succeed,
          stat: (path) => {
            const dev = input.devices?.[path];
            return dev === undefined
              ? Effect.fail(notFound("stat", path))
              : Effect.succeed({ dev } as FileSystem.File.Info);
          },
        }),
        Layer.succeed(HostProcess.Platform, input.platform),
        Layer.succeed(HostProcess.HomeDirectory, input.home),
        Layer.mock(ServerConfig.ServerConfig)({
          stateDir: "/data/onicode/userdata",
          worktreesDir: "/data/onicode/worktrees",
        } as ServerConfig.ServerConfig["Service"]),
        Layer.mock(ServerSettings.ServerSettingsService)({
          getSettings: Effect.succeed({ worktreesDirectory: "" } as never),
        }),
        Layer.mock(ProjectStore.ProjectStoreV2)({
          listShells: () =>
            Effect.succeed(
              (input.projectRoots ?? []).map((workspaceRoot) => ({ workspaceRoot })) as never,
            ),
        }),
        Layer.mock(ProcessRunner.ProcessRunner)({
          run: ({ command }) => {
            const result = input.commands?.[command];
            return result === undefined
              ? Effect.die(`unexpected ${command}`)
              : Effect.succeed({ ...result, stderr: "", timedOut: false } as never);
          },
        }),
        Layer.mock(HostResources.HostResources)({ read: Effect.succeed(usage) }),
      ),
    ),
  );

const read = Effect.flatMap(HostHardware.HostHardware, (hardware) => hardware.read);

describe("HostHardware.layer", () => {
  it.effect("names a Linux machine and lists the volumes Onicode uses", () =>
    Effect.gen(function* () {
      host.cpuModel = "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz";
      host.cpus = 4;
      host.volumes = {
        "/home/user": { blocks: 1000, bavail: 250 },
        "/data/onicode/userdata": { blocks: 4000, bavail: 3000 },
      };

      assert.deepStrictEqual(yield* read, {
        hostname: "build-box",
        model: "LENOVO ThinkPad X1 Carbon Gen 9",
        os: "Ubuntu 24.04.1 LTS",
        cpu: { model: "Intel Core i7-9750H", logicalCores: 4, physicalCores: 2, coreGroups: [] },
        memoryBytes: 32 * GiB,
        uptimeSeconds: 90_061,
        volumes: [
          {
            mountPoint: "/",
            totalBytes: 1000 * 4096,
            availableBytes: 250 * 4096,
            uses: ["home", "projects"],
          },
          {
            mountPoint: "/data",
            totalBytes: 4000 * 4096,
            availableBytes: 3000 * 4096,
            uses: ["data", "worktrees"],
          },
        ],
        usage,
      });
    }).pipe(
      Effect.provide(
        machine({
          platform: "linux",
          home: "/home/user",
          files: {
            "/sys/class/dmi/id/sys_vendor": "LENOVO\n",
            "/sys/class/dmi/id/product_name": "20XW004QUS\n",
            "/sys/class/dmi/id/product_version": "ThinkPad X1 Carbon Gen 9\n",
            "/etc/os-release": 'NAME="Ubuntu"\nPRETTY_NAME="Ubuntu 24.04.1 LTS"\nID=ubuntu\n',
            // Two cores with two threads each.
            "/proc/cpuinfo": [0, 1, 0, 1]
              .map((core, index) => `processor\t: ${index}\nphysical id\t: 0\ncore id\t\t: ${core}`)
              .join("\n\n"),
          },
          devices: {
            "/": 1n,
            "/home": 1n,
            "/home/user": 1n,
            "/home/user/app": 1n,
            "/data": 2n,
            "/data/onicode": 2n,
            "/data/onicode/userdata": 2n,
            "/data/onicode/worktrees": 2n,
          },
          // The second project sits on a drive that is not mounted.
          projectRoots: ["/home/user/app", "/mnt/gone/app"],
        }),
      ),
    ),
  );

  it.effect("reads an Apple silicon Mac's name, system version, and core classes", () =>
    Effect.gen(function* () {
      host.cpuModel = "Apple M4 Pro";
      host.cpus = 14;
      host.volumes = { "/Users/user": { blocks: 1000, bavail: 250 } };
      const hardware = yield* read;

      assert.strictEqual(hardware.model, "Mac mini (2024)");
      assert.strictEqual(hardware.os, "macOS 15.2");
      assert.deepStrictEqual(hardware.cpu, {
        model: "Apple M4 Pro",
        logicalCores: 14,
        physicalCores: 14,
        coreGroups: [
          { name: "Performance", cores: 10 },
          { name: "Efficiency", cores: 4 },
        ],
      });
    }).pipe(
      Effect.provide(
        machine({
          platform: "darwin",
          home: "/Users/user",
          files: {
            "/System/Library/CoreServices/SystemVersion.plist":
              "<dict>\n\t<key>ProductName</key>\n\t<string>macOS</string>\n\t<key>ProductVersion</key>\n\t<string>15.2</string>\n</dict>",
          },
          devices: { "/": 1n, "/Users": 1n, "/Users/user": 1n },
          commands: {
            ioreg: {
              code: 0,
              stdout: '    {\n      "product-name" = <"Mac mini (2024)">\n    }\n',
            },
            // sysctl exits 1 for the perflevel this chip does not have.
            sysctl: {
              code: 1,
              stdout: [
                "hw.physicalcpu: 14",
                "hw.perflevel0.name: Performance",
                "hw.perflevel0.physicalcpu: 10",
                "hw.perflevel1.name: Efficiency",
                "hw.perflevel1.physicalcpu: 4",
              ].join("\n"),
            },
          },
        }),
      ),
    ),
  );
});
