import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import type { ServerSettings as ServerSettingsSnapshot } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Stream from "effect/Stream";

import * as ServerSettings from "../serverSettings.ts";
import * as ComputerDriver from "./ComputerDriver.ts";
import * as ComputerService from "./ComputerService.ts";

// Stands in for cua-driver: `serve` records its arguments and listens until stopped, `call`
// answers check_permissions from a file the test rewrites.
const FAKE_DRIVER = `#!/bin/sh
if [ "$1" = "serve" ]; then
  echo "$*" > "$0.serve"
  echo "Cua Driver daemon listening on fixture" >&2
  exec sleep 600
fi
cat "$0.permissions"
`;

const makeHarness = Effect.fn("test.makeComputerService")(function* (
  options: {
    readonly streamChanges?: (
      settings: ServerSettings.ServerSettingsService["Service"],
    ) => Stream.Stream<ServerSettingsSnapshot>;
  } = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const settings = yield* ServerSettings.ServerSettingsService;
  const directory = yield* fs.makeTempDirectoryScoped({ prefix: "oni-computer-service-test-" });
  const executable = path.join(directory, "cua-driver");
  yield* fs.writeFileString(executable, FAKE_DRIVER, { mode: 0o755 });
  const grant = (granted: boolean) =>
    fs.writeFileString(
      `${executable}.permissions`,
      JSON.stringify({ accessibility: true, screen_recording: granted }),
    );
  yield* grant(true);
  const service = yield* ComputerService.make.pipe(
    Effect.provideService(
      ComputerDriver.ComputerDriver,
      ComputerDriver.ComputerDriver.of({
        version: "1.2.3",
        state: Effect.succeed({ state: "installed" }),
        executable: Effect.succeed(executable),
      }),
    ),
    Effect.provideService(
      ServerSettings.ServerSettingsService,
      ServerSettings.ServerSettingsService.of({
        ...settings,
        ...(options.streamChanges ? { streamChanges: options.streamChanges(settings) } : {}),
      }),
    ),
    // macOS is the platform that reads permissions; its unix socket paths also work on Linux.
    Effect.provideService(HostProcess.Platform, "darwin"),
  );
  const endpoint = service.mcpServer.pipe(
    Effect.map((server) => Option.getOrThrow(server).args.at(-1)),
  );
  return {
    service,
    settings,
    executable,
    grant,
    endpoint,
    serveArguments: fs.readFileString(`${executable}.serve`),
  };
});

const layer = (overrides: Parameters<typeof ServerSettings.ServerSettingsService.layerTest>[0]) =>
  Layer.mergeAll(NodeServices.layer, ServerSettings.ServerSettingsService.layerTest(overrides));

describe.skipIf(HostProcess.Platform.defaultValue() === "win32")("ComputerService", () => {
  it.live(
    "starts the driver in the saved mode and restarts it on a new endpoint when the mode changes",
    () =>
      Effect.gen(function* () {
        const { service, settings, executable, endpoint, serveArguments } = yield* makeHarness();

        const server = Option.getOrThrow(yield* service.mcpServer);

        expect(server.command).toBe(executable);
        expect(server.args.slice(0, 3)).toEqual(["mcp", "--embedded", "--socket"]);
        const standard = yield* serveArguments;
        expect(standard).toContain(`--socket ${server.args[3]}`);
        expect(standard).toContain("--pid-file ");
        expect(standard).toContain("--permission-mode standard");
        expect(standard).not.toContain("--dangerously-bypass-approvals");
        expect(yield* endpoint).toBe(server.args[3]);
        expect((yield* service.status).daemon).toEqual({
          state: "running",
          permissionMode: "standard",
          permissions: { accessibility: true, screenRecording: true },
        });

        yield* settings.updateSettings({ agentComputerPermissionMode: "unrestricted" });

        expect(yield* endpoint).not.toBe(server.args[3]);
        expect(yield* serveArguments).toContain(
          "--permission-mode unrestricted --dangerously-bypass-approvals",
        );
      }).pipe(Effect.scoped, Effect.provide(layer({ enableAgentComputerAccess: true }))),
  );

  it.live("replaces a driver that started without a macOS grant, so a later grant is seen", () =>
    Effect.gen(function* () {
      const { service, grant, endpoint } = yield* makeHarness();
      yield* grant(false);

      const first = yield* endpoint;
      expect((yield* service.status).daemon).toMatchObject({
        permissions: { accessibility: true, screenRecording: false },
      });
      yield* grant(true);

      expect((yield* service.status).daemon).toMatchObject({
        permissions: { accessibility: true, screenRecording: true },
      });
      const granted = yield* endpoint;
      expect(granted).not.toBe(first);
      expect(yield* endpoint).toBe(granted);
    }).pipe(Effect.scoped, Effect.provide(layer({ enableAgentComputerAccess: true }))),
  );

  it.live("stops the driver when computer access is turned off everywhere", () =>
    Effect.gen(function* () {
      const turnedOff = yield* Deferred.make<void>();
      const applied = yield* Deferred.make<void>();
      const { service, settings } = yield* makeHarness({
        // The stream is pulled again only after the service handled the change.
        streamChanges: (base) =>
          Stream.concat(
            Stream.fromEffect(Deferred.await(turnedOff).pipe(Effect.andThen(base.getSettings))),
            Stream.fromEffect(Deferred.succeed(applied, undefined)).pipe(Stream.drain),
          ).pipe(Stream.orDie),
      });
      expect(Option.isSome(yield* service.mcpServer)).toBe(true);

      yield* settings.updateSettings({ enableAgentComputerAccess: false });
      yield* Deferred.succeed(turnedOff, undefined);
      yield* Deferred.await(applied);

      expect((yield* service.status).daemon).toEqual({ state: "stopped" });
    }).pipe(Effect.scoped, Effect.provide(layer({ enableAgentComputerAccess: true }))),
  );
});
