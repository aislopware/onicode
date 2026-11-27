import * as NodeChildProcessSpawner from "@effect/platform-node-shared/NodeChildProcessSpawner";
import * as Context from "effect/Context";
import type * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import * as HostProcess from "@onicode/shared/HostProcess";

import * as NonBlockingSpawner from "./NonBlockingSpawner.ts";

// Each live launcher's current helper; port discovery, telemetry and diagnostics ask synchronously.
const launchers = new Set<Effect.Effect<Option.Option<number>>>();

/** Whether this is one of the server's launchers, which hosts its commands and agents. */
export const isLauncher = (pid: number) =>
  [...launchers].some((currentPid) => Option.contains(Effect.runSync(currentPid), pid));

/** Whether a process with this parent was spawned by this server, directly or through its launcher. */
export const isServerChild = (ppid: number) => ppid === process.pid || isLauncher(ppid);

// On Linux, libuv forks the whole server for every spawn and blocks the event loop while the
// kernel copies its page tables, so commands are launched by a small long-lived helper instead.
export const make = Effect.fn("ProcessLauncher.make")(function* (responseTimeout?: Duration.Input) {
  const fallback = yield* ChildProcessSpawner.ChildProcessSpawner;
  if ((yield* HostProcess.Platform) !== "linux") return fallback;

  const helper = NodeChildProcessSpawner.layerProxyProcess({
    executable: yield* HostProcess.ExecutablePath,
    // A single executable cannot run `-e`, so it hosts the helper as a hidden subcommand of bin.ts.
    args: (yield* HostProcess.IsExecutable) ? ["__process-launcher"] : undefined,
    env: {
      ...(yield* HostProcess.Environment),
      ELECTRON_RUN_AS_NODE: "1",
      NODE_OPTIONS: undefined,
      // `node --watch` children would report their imports over the helper's IPC channel.
      WATCH_REPORT_DEPENDENCIES: undefined,
    },
    responseTimeout,
  });
  // The trampoline keeps a hung cwd or PATH entry from stalling the helper, and with it every command.
  // Fresh: the server has already built this layer around the forking spawner.
  const launched = Layer.fresh(NonBlockingSpawner.layer).pipe(Layer.provideMerge(helper));
  return yield* Layer.build(launched).pipe(
    Effect.flatMap((context) => {
      const { currentPid } = Context.get(context, NodeChildProcessSpawner.ProxyProcess);
      return Effect.acquireRelease(
        Effect.sync(() => launchers.add(currentPid)),
        () => Effect.sync(() => launchers.delete(currentPid)),
      ).pipe(Effect.as(Context.get(context, ChildProcessSpawner.ChildProcessSpawner)));
    }),
    Effect.catch((error) =>
      Effect.as(
        Effect.logWarning("The process launcher did not start; commands fork the server.", error),
        fallback,
      ),
    ),
  );
});

/**
 * The server's one spawner: memoized, so every build site shares a single launcher. The provided
 * spawner stays the fallback when the launcher cannot start.
 */
export const layerServer = Layer.effect(ChildProcessSpawner.ChildProcessSpawner, make()).pipe(
  Layer.provide(NonBlockingSpawner.layer),
);
