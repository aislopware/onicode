import type { ComputerDriverInstall } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";

import * as ServerConfig from "../config.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";
import PINNED_TOOLS from "../tools/pinnedTools.json" with { type: "json" };

// pinnedTools.json holds the version, sizes and hashes of the `-binary` assets of
// https://github.com/trycua/cua/releases (tag cua-driver-rs-v<version>). The daemon and
// every MCP proxy run this one binary; the proxy refuses a daemon of another version.
const { version: VERSION, assets: ARCHIVES } = PINNED_TOOLS["cua-driver"];

const driverPlatform = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): keyof typeof ARCHIVES | null => {
  switch (platform) {
    case "darwin":
      return arch === "arm64" || arch === "x64" ? "darwin-universal" : null;
    case "linux":
      return arch === "x64" ? "linux-x86_64" : arch === "arm64" ? "linux-arm64" : null;
    case "win32":
      return arch === "x64" ? "windows-x86_64" : arch === "arm64" ? "windows-arm64" : null;
    default:
      return null;
  }
};

export type ComputerDriverRelease = PinnedTool.PinnedToolRelease;

const computerDriverRelease = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): ComputerDriverRelease | null => {
  const target = driverPlatform(platform, arch);
  if (target === null) return null;
  const extension = target.startsWith("windows") ? "zip" : "tar.gz";
  return {
    version: VERSION,
    url: `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${VERSION}/cua-driver-rs-${VERSION}-${target}-binary.${extension}`,
    ...ARCHIVES[target],
  };
};

// The archives also carry the SDK library, its Node addon and a C header, which Onicode never loads.
const SKIPPED_ARCHIVE_ENTRIES = ["*cua_driver_sdk*", "*.node", "*.h"];

export class ComputerDriverInstallError extends Schema.TaggedError<ComputerDriverInstallError>()(
  "ComputerDriverInstallError",
  { detail: Schema.String, cause: Schema.optional(Schema.Defect()) },
) {
  override get message(): string {
    return `Onicode could not install cua-driver: ${this.detail}`;
  }
}

export class ComputerDriverUnsupportedError extends Schema.TaggedError<ComputerDriverUnsupportedError>()(
  "ComputerDriverUnsupportedError",
  { platform: Schema.String, arch: Schema.String },
) {
  override get message(): string {
    return `cua-driver has no build for ${this.platform}-${this.arch}.`;
  }
}

export class ComputerDriver extends Context.Service<
  ComputerDriver,
  {
    readonly version: string;
    /** Never starts an install. */
    readonly state: Effect.Effect<ComputerDriverInstall>;
    /** Installs the driver when it is missing, and waits for that install. A failed install is retried. */
    readonly executable: Effect.Effect<
      string,
      ComputerDriverInstallError | ComputerDriverUnsupportedError
    >;
  }
>()("onicode/computer/ComputerDriver") {}

export interface ComputerDriverOptions {
  readonly baseDir: string;
  readonly release?: ComputerDriverRelease | null;
}

export const makeComputerDriver = Effect.fn("ComputerDriver.make")(function* (
  options: ComputerDriverOptions,
) {
  const platform = yield* HostProcess.Platform;
  const arch = yield* HostProcess.Architecture;
  const release =
    options.release === undefined ? computerDriverRelease(platform, arch) : options.release;
  const tool = yield* PinnedTool.makePinnedTool({
    name: "cua-driver",
    baseDir: options.baseDir,
    executableName: platform === "win32" ? "cua-driver.exe" : "cua-driver",
    release,
    payload: { kind: "archive", exclude: SKIPPED_ARCHIVE_ENTRIES },
  });
  return ComputerDriver.of({
    version: release?.version ?? VERSION,
    state: tool.state,
    executable: tool.executable.pipe(
      Effect.mapError((error) =>
        error._tag === "PinnedToolUnsupportedError"
          ? new ComputerDriverUnsupportedError({ platform: error.platform, arch: error.arch })
          : new ComputerDriverInstallError({ detail: error.detail, cause: error.cause }),
      ),
    ),
  });
});

export const layer = Layer.effect(
  ComputerDriver,
  Effect.gen(function* () {
    const config = yield* ServerConfig.ServerConfig;
    return yield* makeComputerDriver({ baseDir: config.baseDir });
  }),
);
