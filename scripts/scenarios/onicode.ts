// `test` for desktop scenarios: e2e's web test, driving the dev desktop window, plus `onicode`,
// which seeds state and opens the app. `oni-mobile.ts` is the same for the iOS app.
import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeWS from "@effect/platform-node/NodeWS";
import { test as base } from "@e2e-dev/web";
import {
  CommandId,
  EnvironmentHttpApi,
  ORCHESTRATION_PROTOCOL_VERSION,
  type ProjectMutation,
  ProjectId,
  WsRpcGroup,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as HttpApiClient from "effect/http-api/HttpApiClient";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Schedule from "effect/Schedule";
import type * as Scope from "effect/Scope";
import * as Schema from "effect/Schema";
import {
  FetchHttpClient,
  HttpClient,
  type HttpClientError,
  HttpClientRequest,
  HttpClientResponse,
} from "effect/http";
import { RpcClient, type RpcClientError, RpcSerialization } from "effect/rpc";
import { Socket } from "effect/socket";

import { APP_DIR, ONICODE_HOME } from "../e2e.config.ts";
import { run, ScenarioError } from "./shell.ts";

export interface OniState {
  /** Creates a git repository holding `files` and adds it as a project. Returns its path. */
  readonly project: (
    title: string,
    files?: Readonly<Record<string, string | Uint8Array>>,
  ) => Promise<string>;
  /**
   * Starts a thread in a project and plays one replayed turn per message before returning.
   * `model` is a dev replay recording: showcase, todo-list, approval, question, multi-turn,
   * command-output.
   */
  readonly thread: (input: {
    readonly project: string;
    readonly title: string;
    readonly messages: ReadonlyArray<string>;
    readonly model?: string;
  }) => Promise<OniThread>;
}

export interface OniThread {
  readonly threadId: string;
}

export interface Onicode extends OniState {
  /**
   * Opens the app at a path or on a thread `onicode.thread` returned. Opening the thread
   * by its link is the reliable way to reach it: a sidebar row covers its own title text.
   */
  readonly open: (target?: string | OniThread) => Promise<void>;
}

/** e2e's fixtures are promise functions; this is the one place an Effect becomes a promise. */
export const runPromise = <A, E>(effect: Effect.Effect<A, E, NodeServices.NodeServices>) =>
  Effect.runPromise(effect.pipe(Effect.provide(NodeServices.layer)));

const decodeLaunched = Schema.decodeUnknownEffect(
  Schema.fromJsonString(Schema.Struct({ threadId: Schema.String })),
);

const decodeRuntimeState = Schema.decodeUnknownEffect(
  Schema.fromJsonString(Schema.Struct({ origin: Schema.String, startedAt: Schema.String })),
);

type ServerApi = {
  readonly client: HttpApiClient.ForApi<typeof EnvironmentHttpApi>;
  readonly headers: { readonly authorization: string };
};

/**
 * One API client per running server, signed in once. Each CLI call starts a process that takes
 * about 0.7 s before it does anything, so per-test state goes through the server's HTTP API. A
 * server started again on the same home, as the second server is for each test, gets a new one.
 */
const serverApis = new Map<string, Promise<ServerApi>>();

const uuid = SecureRandom.SecureRandom.pipe(
  Effect.flatMap((random) => random.uuidV4()),
  Effect.provide(NodeCrypto.layer),
);

/** State for the server on `home`, made through the app's own CLI and API: real events, not inserted rows. */
export function makeState(home: string, env: NodeJS.ProcessEnv = {}) {
  const cli = Effect.fn("scenario.cli")(function* (
    args: ReadonlyArray<string>,
    allowExit: ReadonlyArray<number> = [0],
  ) {
    const hostEnvironment = yield* HostProcess.Environment;
    return yield* run(
      yield* HostProcess.ExecutablePath,
      ["apps/server/src/bin.ts", ...args, "--base-dir", home],
      {
        cwd: APP_DIR,
        env: { PATH: hostEnvironment.PATH, HOME: hostEnvironment.HOME, ...env },
        allowExit,
      },
    );
  });

  // The server writes its origin beside its database once it listens. It also prints its pairing
  // token, which can come first, so a missing file is read again.
  const runtimeState = Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    return yield* decodeRuntimeState(
      yield* fs.readFileString(path.join(home, "userdata", "server-runtime.json")),
    );
  }).pipe(
    Effect.retry({
      schedule: Schedule.spaced("50 millis"),
      times: 100,
      while: (error) => error._tag === "PlatformError" && error.reason._tag === "NotFound",
    }),
  );
  // A server that is starting, or starting again, refuses requests with 503 until it serves.
  const serving = Effect.gen(function* () {
    const { origin } = yield* runtimeState;
    yield* HttpClient.get(`${origin}/.well-known/onicode/environment`).pipe(
      Effect.flatMap(HttpClientResponse.filterStatusOk),
    );
  }).pipe(
    Effect.retry({ schedule: Schedule.spaced("250 millis"), times: 120 }),
    Effect.mapError((cause) => new ScenarioError({ message: `The server never served: ${cause}` })),
    Effect.provide(FetchHttpClient.layer),
  );
  const connect = Effect.fn("scenario.connect")(function* (origin: string) {
    const issued = yield* cli(["auth", "session", "issue", "--token-only", "--label", "scenario"]);
    const client = yield* HttpApiClient.make(EnvironmentHttpApi, { baseUrl: origin });
    return { client, headers: { authorization: `Bearer ${issued.stdout.trim()}` } };
  }, Effect.provide(FetchHttpClient.layer));
  const api = Effect.gen(function* () {
    const { origin, startedAt } = yield* runtimeState;
    const key = `${home}\n${origin}\n${startedAt}`;
    return yield* Effect.tryPromise({
      try: () => {
        const known = serverApis.get(key);
        if (known !== undefined) return known;
        const connecting = runPromise(connect(origin));
        serverApis.set(key, connecting);
        // A failed sign-in is not kept: the next test tries again.
        connecting.catch(() => serverApis.delete(key));
        return connecting;
      },
      catch: (cause) => new ScenarioError({ message: `Could not reach the server API: ${cause}` }),
    });
  });
  const mutate = Effect.fn("scenario.mutate")(function* (mutation: ProjectMutation) {
    const { client, headers } = yield* api;
    return yield* client.projects
      .mutate({ headers, payload: mutation })
      .pipe(
        Effect.mapError((cause) => new ScenarioError({ message: `${mutation.type}: ${cause}` })),
      );
  });
  const projects: Array<ProjectId> = [];

  const project = Effect.fn("scenario.project")(function* (
    title: string,
    files: Readonly<Record<string, string | Uint8Array>>,
  ) {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    // Its own folder per test: the server and this home outlive one test.
    yield* fs.makeDirectory(path.join(home, "workspaces"), { recursive: true });
    const parent = yield* fs.makeTempDirectory({
      directory: path.join(home, "workspaces"),
      prefix: "w-",
    });
    const workspace = path.join(parent, title);
    for (const [file, content] of Object.entries({ "README.md": "", ...files })) {
      yield* fs.makeDirectory(path.dirname(path.join(workspace, file)), { recursive: true });
      yield* typeof content === "string"
        ? fs.writeFileString(path.join(workspace, file), content)
        : fs.writeFile(path.join(workspace, file), content);
    }
    const git = (args: ReadonlyArray<string>) => run("git", args, { cwd: workspace });
    const author = ["-c", "user.name=Scenario", "-c", "user.email=scenario@example.invalid"];
    yield* git(["init", "--quiet", "--initial-branch=main"]);
    // The developer's global config may sign commits with a key that prompts.
    yield* git(["config", "commit.gpgsign", "false"]);
    yield* git(["add", "--all"]);
    yield* git([...author, "commit", "--quiet", "--message", "Initial commit"]);
    const created = yield* mutate({
      type: "project.create",
      commandId: CommandId.make(yield* uuid),
      projectId: ProjectId.make(yield* uuid),
      title,
      workspaceRoot: workspace,
    });
    projects.push(created.id);
    return workspace;
  });

  const thread = Effect.fn("scenario.thread")(function* (input: {
    readonly project: string;
    readonly title: string;
    readonly messages: ReadonlyArray<string>;
    readonly model?: string;
  }) {
    // Exit code 2 means the turn waits for an answer, which a scenario may want to start from.
    const turn = (args: ReadonlyArray<string>) =>
      cli(["thread", ...args, "--wait", "--json"], [0, 2]);
    const [first = "Start", ...rest] = input.messages;
    const replay = ["--provider", "devReplay", "--model", input.model ?? "multi-turn"];
    // The CLI does not retry a start it could not deliver, since one it did deliver would repeat.
    yield* serving;
    const started = yield* turn(["start", input.project, first, "--title", input.title, ...replay]);
    const { threadId } = yield* decodeLaunched(started.stdout);
    for (const message of rest) yield* turn(["send", threadId, message]);
    return { threadId } satisfies OniThread;
  });

  return {
    cli,
    state: {
      project: (title, files = {}) => runPromise(project(title, files)),
      thread: (input) => runPromise(thread(input)),
    } satisfies OniState,
    // The server outlives the test, so the next one must not see this one's projects, including
    // those it added through the app from inside its workspaces.
    cleanup: Effect.gen(function* () {
      if (projects.length === 0) return;
      const { client, headers } = yield* api;
      const { projects: live } = yield* client.projects
        .snapshot({ headers })
        .pipe(Effect.mapError((cause) => new ScenarioError({ message: `projects: ${cause}` })));
      const path = yield* Path.Path;
      // Folders it added live under workspaces; projects it created live under projects.
      const roots = ["workspaces", "projects"].map((root) => `${path.join(home, root)}/`);
      const added = live.flatMap((project) =>
        project.deletedAt === null && roots.some((root) => project.workspaceRoot.startsWith(root))
          ? [project.id]
          : [],
      );
      yield* Effect.forEach(
        new Set([...projects, ...added]),
        (projectId) =>
          Effect.flatMap(uuid, (commandId) =>
            mutate({
              type: "project.delete",
              commandId: CommandId.make(commandId),
              projectId,
              force: true,
            }),
          ),
        { discard: true },
      );
    }),
  };
}

/** The id every thread link starts with, asked of the server the app talks to. */
export const environmentId = (baseUrl: string) =>
  HttpClient.get(`${baseUrl}/.well-known/onicode/environment`).pipe(
    Effect.flatMap(HttpClientResponse.filterStatusOk),
    Effect.flatMap(
      HttpClientResponse.schemaBodyJson(Schema.Struct({ environmentId: Schema.String })),
    ),
    Effect.map((environment) => environment.environmentId),
    Effect.provide(FetchHttpClient.layer),
  );

/** The typed RPC client of the server on `home` at `origin`, signed in through its own CLI. */
// Annotated: Onicode's WsRpcGroup is too large for the inferred type to be emitted (TS7056).
export const rpcClient: (
  home: string,
  origin: string,
) => Effect.Effect<
  RpcClient.FromGroup<typeof WsRpcGroup, RpcClientError.RpcClientError>,
  HttpClientError.HttpClientError | ScenarioError | Schema.SchemaError,
  Scope.Scope | NodeServices.NodeServices
> = Effect.fn("scenario.rpcClient")(function* (home: string, origin: string) {
  const issued = yield* makeState(home).cli(["auth", "session", "issue", "--token-only"]);
  const { ticket } = yield* HttpClient.execute(
    HttpClientRequest.post(`${origin}/api/auth/websocket-ticket`).pipe(
      HttpClientRequest.bearerToken(issued.stdout.trim()),
    ),
  ).pipe(
    Effect.flatMap(HttpClientResponse.filterStatusOk),
    Effect.flatMap(HttpClientResponse.schemaBodyJson(Schema.Struct({ ticket: Schema.String }))),
    Effect.provide(FetchHttpClient.layer),
  );
  // Built into the caller's scope: `Effect.provide` would close the socket once `make` returns.
  const protocol = yield* Layer.build(
    RpcClient.layerProtocolSocket().pipe(
      Layer.provide(
        Socket.layerWebSocket(
          `${origin.replace(/^http/, "ws")}/ws?orchestrationProtocol=${ORCHESTRATION_PROTOCOL_VERSION}&wsTicket=${encodeURIComponent(ticket)}`,
        ),
      ),
      Layer.provide(RpcSerialization.layerJson),
      Layer.provide(NodeWS.layerWebSocketConstructor),
    ),
  );
  return yield* RpcClient.make(WsRpcGroup).pipe(Effect.provideContext(protocol));
});

export const test = base.extend<{ onicode: Onicode }>({
  onicode: async ({ app, browser }, use) => {
    const { state, cleanup } = makeState(ONICODE_HOME);
    await use({
      ...state,
      // The desktop app is its own server's client, so there is no pairing page to sign in on.
      open: async (target = "/") => {
        if (typeof target === "string") await app.open(target);
        else {
          const environment = await runPromise(environmentId(app.baseUrl ?? ""));
          await app.open(`/${environment}/${target.threadId}`);
        }
        // Shortcuts and native menus reach the app only once it mounts, and the reload through
        // Vite can outlast a default wait. Not an `expect`: a proof counts a failed assertion as
        // the bug. A narrow window collapses the sidebar to its toggle. Attributes, not names,
        // because a scenario may have switched the app's language.
        await browser
          .locator('[data-app-sidebar][role="navigation"], [data-sidebar="trigger"]')
          .first()
          .waitFor({ timeout: 60_000 });
      },
    });
    await runPromise(cleanup);
  },
});

/**
 * Test option for a desktop scenario: the iOS target skips it. The desktop target's platform is
 * "web", since e2e drives the window's renderer as a web page.
 */
export const WEB = { platforms: ["web"] } as const;
