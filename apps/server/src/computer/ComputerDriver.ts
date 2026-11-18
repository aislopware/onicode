import type { ComputerDriverInstall } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";

import * as ServerConfig from "../config.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";

// Update the version, sizes and hashes together from the `-binary` assets of
// https://github.com/trycua/cua/releases (tag cua-driver-rs-v<version>). The daemon and
// every MCP proxy run this one binary; the proxy refuses a daemon of another version.
const VERSION = "0.49.11";
const ARCHIVES = {
  "darwin-universal": {
    extension: "tar.gz",
    bytes: 43_201_581,
    sha256: "fcd35de80b31b7144839a227b0dbfb1d691adf2ddcd0a2588be9a0128ec2e582",
  },
  "linux-x86_64": {
    extension: "tar.gz",
    bytes: 29_987_915,
    sha256: "d6566ba4b799fb7a161cfa9a94b3cd95fb1b0f766d98fa4888a9c2dec1ffb076",
  },
  "linux-arm64": {
    extension: "tar.gz",
    bytes: 27_911_664,
    sha256: "ec189e9708436976ea3e6df43aaf35f1c665590fb0141d4da3776e07d0ea1adb",
  },
  "windows-x86_64": {
    extension: "zip",
    bytes: 32_317_826,
    sha256: "e5fe14e2a7da5ba7a2aa116f706bfb7c078ecd922819ee9587d18cc07ae2a24c",
  },
  "windows-arm64": {
    extension: "zip",
    bytes: 30_079_478,
    sha256: "2df64cb4ca0477d176787fc0c5c35ceb9fcb829962a0f20fa07f170529098c85",
  },
} as const;

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
  const { extension, bytes, sha256 } = ARCHIVES[target];
  return {
    version: VERSION,
    url: `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${VERSION}/cua-driver-rs-${VERSION}-${target}-binary.${extension}`,
    bytes,
    sha256,
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
