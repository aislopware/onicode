// A second Onicode server for scenarios about remote environments: its own home, the replay agent,
// and a pairing code for the desktop app's Add environment dialog.
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as NetService from "@onicode/shared/Net";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Path from "effect/Path";

import { APP_DIR } from "../e2e.config.ts";
import { makeState } from "./onicode.ts";
import { ENVIRONMENT_LABEL, prepareHome, start } from "./start-app.ts";

interface Pairing {
  readonly host: string;
  readonly token: string;
}

const serve = Effect.fn("scenario.secondEnvironment")(function* (
  home: string,
  paired: (pairing: Pairing) => void,
) {
  const path = yield* Path.Path;
  const node = yield* HostProcess.ExecutablePath;
  const hostEnvironment = yield* HostProcess.Environment;
  const port = yield* (yield* NetService.NetService).reserveLoopbackPort();
  yield* prepareHome(home);
  const token = yield* Deferred.make<string>();
  yield* start({
    cwd: APP_DIR,
    command: node,
    args: [
      "apps/server/src/bin.ts",
      "serve",
      "--host",
      "127.0.0.1",
      "--port",
      String(port),
      "--base-dir",
      home,
      "--dev",
    ],
    env: {
      PATH: `${path.join(home, "bin")}:${hostEnvironment.PATH ?? ""}`,
      HOME: hostEnvironment.HOME,
      ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD: "0",
      ONICODE_ENVIRONMENT_LABEL: ENVIRONMENT_LABEL,
    },
    onLine: (line) => {
      const value = /^Token:\s*(\S+)/.exec(line)?.[1];
      return value === undefined ? Effect.void : Deferred.succeed(token, value).pipe(Effect.asVoid);
    },
  });
  paired({ host: `http://127.0.0.1:${port}`, token: yield* Deferred.await(token) });
  return yield* Effect.never;
});

/** Starts the server and resolves once it can be paired. `stop` ends it. */
export const startSecondEnvironment = async (home: string) => {
  const pairing = Promise.withResolvers<Pairing>();
  const server = Effect.runFork(
    serve(home, pairing.resolve).pipe(
      Effect.scoped,
      Effect.provide([NodeServices.layer, NetService.layer]),
    ),
  );
  server.addObserver(() => pairing.reject(new Error("The second server stopped.")));
  const stop = () => Effect.runPromise(Fiber.interrupt(server));
  try {
    return { ...(await pairing.promise), ...makeState(home), stop };
  } catch (cause) {
    await stop();
    throw cause;
  }
};
