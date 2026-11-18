import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId, ProviderInstanceId, ThreadId } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";

import * as ServerConfig from "../config.ts";
import * as PreviewCdpBridge from "../preview/PreviewCdpBridge.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as PinnedTool from "../tools/PinnedTool.ts";
import * as BrowserUseDriver from "./BrowserUseDriver.ts";
import * as BrowserUseService from "./BrowserUseService.ts";

// Stands in for obu: records each invocation with the session it was run for.
const FAKE_OBU = `#!/bin/sh
echo "$* $OBU_SESSION" >> "$0.calls"
`;

const threadId = ThreadId.make("browser-use-thread");
const session = BrowserUseService.browserUseSessionName(threadId);
const scope = {
  environmentId: EnvironmentId.make("browser-use-environment"),
  thread: {
    threadId,
    providerSessionId: "agent-a",
    providerInstanceId: ProviderInstanceId.make("claude"),
  },
  client: undefined,
  requestNamespace: "browser-use",
  capabilities: new Set(["preview", "preview-driver"] as const),
  issuedAt: 1,
};

const makeHarness = Effect.fn("test.makeBrowserUseService")(function* (
  driver?: Partial<Pick<BrowserUseDriver.BrowserUseDriver["Service"], "state" | "executable">>,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const config = yield* ServerConfig.ServerConfig;
  const executable = path.join(config.baseDir, "obu");
  yield* fs.writeFileString(executable, FAKE_OBU, { mode: 0o755 });
  const released: Array<string> = [];
  const service = yield* BrowserUseService.make.pipe(
    Effect.provideService(
      BrowserUseDriver.BrowserUseDriver,
      BrowserUseDriver.BrowserUseDriver.of({
        version: "1.2.3",
        state: Effect.succeed({ state: "installed" }),
        executable: Effect.succeed(executable),
        ...driver,
      }),
    ),
    Effect.provideService(
      PreviewCdpBridge.PreviewCdpBridge,
      PreviewCdpBridge.PreviewCdpBridge.of({
        endpoint: () => Effect.succeed({ host: "127.0.0.1", port: 4567, path: "/k/secret" }),
        release: (released_) => Effect.sync(() => void released.push(released_)),
        bind: () => Effect.void,
      }),
    ),
  );
  const home = path.join(config.stateDir, "browser-use");
  return {
    service,
    fs,
    path,
    executable,
    home,
    released,
    server: (target: "panel" | "isolated" | "personal", port: number | null = null) =>
      service.mcpServer({ scope, target, port }).pipe(Effect.map(Option.getOrThrow)),
    calls: fs.readFileString(`${executable}.calls`).pipe(Effect.orElseSucceed(() => "")),
  };
});

const layer = Layer.mergeAll(
  ServerSettings.ServerSettingsService.layerTest(),
  ServerConfig.layerTest(process.cwd(), { prefix: "oni-browser-use-service-test-" }),
).pipe(Layer.provideMerge(NodeServices.layer));

describe.skipIf(HostProcess.Platform.defaultValue() === "win32")("BrowserUseService", () => {
  it.effect("starts obu for one thread against the browser the setting names", () =>
    Effect.gen(function* () {
      const { server, fs, path, executable, home } = yield* makeHarness();
      const shared = { OBU_HOME: home, OBU_SESSION: session, OBU_NO_UPDATE_CHECK: "1" };

      expect(yield* server("panel")).toEqual({
        command: executable,
        args: ["mcp", "--host", "127.0.0.1", "--port", "4567"],
        env: {
          ...shared,
          OBU_LANE: "real",
          OBU_ENDPOINT_PATH: "/k/secret",
          OBU_HOST_NOTE: expect.stringContaining("Onicode's Browser panel"),
        },
      });
      expect(yield* server("isolated")).toEqual({
        command: executable,
        args: ["mcp"],
        env: { ...shared, OBU_LANE: "agent", OBU_HOST_NOTE: expect.stringContaining("your own") },
      });

      // The personal browser's port lives in obu's own config, beside what the user set there.
      const realBrowser = path.join(home, "realbrowser.json");
      expect(yield* server("personal")).toEqual({
        command: executable,
        args: ["mcp"],
        env: { ...shared, OBU_LANE: "real" },
      });
      expect(yield* fs.exists(realBrowser)).toBe(false);
      yield* server("personal", 9555);
      expect(JSON.parse(yield* fs.readFileString(realBrowser))).toEqual({ port: 9555 });
      yield* fs.writeFileString(realBrowser, `{"mode":"attach","port":9555}`);
      yield* server("personal", 9556);
      expect(JSON.parse(yield* fs.readFileString(realBrowser))).toEqual({
        mode: "attach",
        port: 9556,
      });
      yield* server("personal");
      expect(JSON.parse(yield* fs.readFileString(realBrowser))).toEqual({ mode: "attach" });
    }).pipe(Effect.scoped, Effect.provide(layer)),
  );

  it.effect("starts a session without obu when it cannot be installed", () =>
    Effect.gen(function* () {
      const unsupported = yield* makeHarness({ state: Effect.succeed({ state: "unsupported" }) });
      expect(yield* unsupported.service.ready).toBe(false);

      let attempt: BrowserUseDriver.BrowserUseDriver["Service"]["executable"] = Effect.fail(
        new PinnedTool.PinnedToolInstallError({ tool: "obu", detail: "No network." }),
      );
      const { service } = yield* makeHarness({
        state: Effect.succeed({ state: "missing" }),
        executable: Effect.suspend(() => attempt),
      });
      expect(yield* service.ready).toBe(false);
      // A later session does not wait on a download that hangs.
      attempt = Effect.never;
      expect(yield* service.ready).toBe(false);
    }).pipe(Effect.scoped, Effect.provide(layer)),
  );

  it.live("closes an ended thread's tabs, and forgets a deleted thread", () =>
    Effect.gen(function* () {
      const { service, fs, path, home, released, calls } = yield* makeHarness();
      const record = path.join(home, "agent-sessions", session);

      // A driver that never attached to a browser left nothing to close.
      yield* service.endSession(threadId, { forget: true });
      expect(yield* calls).toBe("");
      expect(released).toEqual([threadId]);

      yield* fs.makeDirectory(record, { recursive: true });
      yield* service.endSession(threadId, { forget: false });
      expect(yield* calls).toBe(`stop --close-tabs ${session}\n`);
      expect(yield* fs.exists(record)).toBe(true);

      yield* service.endSession(threadId, { forget: true });
      expect(yield* fs.exists(record)).toBe(false);
      expect(released).toHaveLength(3);
    }).pipe(Effect.scoped, Effect.provide(layer)),
  );
});
