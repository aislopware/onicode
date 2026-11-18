import type { ComputerDriverInstall } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerConfig from "../config.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";

// Update the version, sizes and hashes together from the assets of
// https://github.com/aislopware/oni-browser-use/releases (tag v<version>).
const VERSION = "4.8.1";
const BINARIES = {
  "darwin-arm64": {
    bytes: 7_186_880,
    sha256: "1e9643bd93b39e865c2fdf443194aa11e779b5349f3a68282e71a51c8e498e33",
  },
  "linux-x64": {
    bytes: 8_460_336,
    sha256: "63cbac9b0a6ba5211b1949b116995b697d185c512414a68e76574484aba08dd0",
  },
} as const;

const driverPlatform = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): keyof typeof BINARIES | null => {
  // The fork publishes these two builds and no others.
  if (platform === "darwin" && arch === "arm64") return "darwin-arm64";
  if (platform === "linux" && arch === "x64") return "linux-x64";
  return null;
};

const browserUseRelease = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): PinnedTool.PinnedToolRelease | null => {
  const target = driverPlatform(platform, arch);
  if (target === null) return null;
  return {
    version: VERSION,
    url: `https://github.com/aislopware/oni-browser-use/releases/download/v${VERSION}/obu-${target}`,
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
    executableName: "obu",
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
