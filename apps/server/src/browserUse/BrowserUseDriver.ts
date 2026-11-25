import type { ComputerDriverInstall } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerConfig from "../config.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";
import PINNED_TOOLS from "../tools/pinnedTools.json" with { type: "json" };

// pinnedTools.json holds the version, sizes and hashes of the assets of
// https://github.com/aislopware/oni-browser-use/releases (tag v<version>).
export const { version: VERSION } = PINNED_TOOLS.obu;
const BINARIES = PINNED_TOOLS.obu.assets;

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
