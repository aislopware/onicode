import type { ComputerDriverInstall } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerConfig from "../config.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";

// Update the version, sizes and hashes together from the assets of
// https://github.com/aislopware/oni-browser-use/releases (tag v<version>).
export const VERSION = "4.9.1";
const BINARIES = {
  "darwin-arm64": {
    bytes: 7_220_112,
    sha256: "b96e0339bbe1548ef6f799ac3754b54e6a12f8c2a8fbc80189506ae726cd71ab",
  },
  "linux-x64": {
    bytes: 8_504_504,
    sha256: "40088813219497467b0ceaa4430105919829938b28dc0adec50fdb96c58439d1",
  },
  "windows-x64": {
    bytes: 7_935_488,
    sha256: "093af399cd3dc2db379129e4be820187a7513322baf1ad32a9cac79aa1a69ade",
  },
} as const;

const driverPlatform = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): keyof typeof BINARIES | null => {
  // The fork publishes these three builds and no others.
  if (platform === "darwin" && arch === "arm64") return "darwin-arm64";
  if (platform === "linux" && arch === "x64") return "linux-x64";
  if (platform === "win32" && arch === "x64") return "windows-x64";
  return null;
};

export const browserUseRelease = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): PinnedTool.PinnedToolRelease | null => {
  const target = driverPlatform(platform, arch);
  if (target === null) return null;
  return {
    version: VERSION,
    url: `https://github.com/aislopware/oni-browser-use/releases/download/v${VERSION}/obu-${target}${target === "windows-x64" ? ".exe" : ""}`,
    ...BINARIES[target],
  };
};

export class BrowserUseDriver extends Context.Service<
  BrowserUseDriver,
  {
    readonly version: string;
    /** Never starts an install. */
    readonly state: Effect.Effect<ComputerDriverInstall>;
    /** Installs obu when it is missing, and waits for that install. A failed install is retried. */
    readonly executable: Effect.Effect<
      string,
      PinnedTool.PinnedToolInstallError | PinnedTool.PinnedToolUnsupportedError
    >;
  }
>()("onicode/browserUse/BrowserUseDriver") {}

export interface BrowserUseDriverOptions {
  readonly baseDir: string;
  readonly release?: PinnedTool.PinnedToolRelease | null;
}

export const makeBrowserUseDriver = Effect.fn("BrowserUseDriver.make")(function* (
  options: BrowserUseDriverOptions,
) {
  const platform = yield* HostProcess.Platform;
  const arch = yield* HostProcess.Architecture;
  const release =
    options.release === undefined ? browserUseRelease(platform, arch) : options.release;
  const tool = yield* PinnedTool.makePinnedTool({
    name: "obu",
    baseDir: options.baseDir,
    executableName: platform === "win32" ? "obu.exe" : "obu",
    release,
    payload: { kind: "binary" },
  });
  return BrowserUseDriver.of({ version: release?.version ?? VERSION, ...tool });
});

export const layer = Layer.effect(
  BrowserUseDriver,
  Effect.gen(function* () {
    const config = yield* ServerConfig.ServerConfig;
    return yield* makeBrowserUseDriver({ baseDir: config.baseDir });
  }),
);
