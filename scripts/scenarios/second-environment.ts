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
import { prepareHome, start } from "./start-app.ts";

interface Pairing {
  readonly host: string;
  readonly token: string;
  /** The name the server reports for itself, which the app lists it under. */
  readonly label: string;
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
    },
    onLine: (line) => {
      const value = /^Token:\s*(\S+)/.exec(line)?.[1];
      return value === undefined ? Effect.void : Deferred.succeed(token, value).pipe(Effect.asVoid);
    },
  });
  const host = `http://127.0.0.1:${port}`;
  const code = yield* Deferred.await(token);
  const { label } = yield* Effect.promise(async () => {
    // @effect-diagnostics-next-line globalFetchInEffect:off - one read of the server's own description
    const response = await fetch(`${host}/.well-known/onicode/environment`);
    return (await response.json()) as { readonly label: string };
  });
  paired({ host, token: code, label });
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
