// `test` for desktop scenarios: e2e's web test, driving the dev desktop window, plus `onicode`,
// which seeds state and opens the app. `oni-mobile.ts` is the same for the iOS app.
import * as NodeServices from "@effect/platform-node/NodeServices";
import { test as base } from "@e2e-dev/web";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import { FetchHttpClient, HttpClient, HttpClientResponse } from "effect/http";

import { APP_DIR, ONICODE_HOME } from "../e2e.config.ts";
import { run } from "./shell.ts";

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

/** State for the server on `home`, made through the app's own CLI: real events, not inserted rows. */
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
  const projects: Array<string> = [];

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
    yield* cli(["project", "add", workspace, "--title", title]);
    projects.push(workspace);
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
    // The server outlives the test, so the next one must not see this one's projects.
    cleanup: Effect.forEach(
      projects,
      (workspace) => cli(["project", "remove", workspace, "--force"]),
      { discard: true },
    ),
  };
}

/** The id every thread link starts with, asked of the server the app talks to. */
const environmentId = (baseUrl: string) =>
  HttpClient.get(`${baseUrl}/.well-known/onicode/environment`).pipe(
    Effect.flatMap(HttpClientResponse.filterStatusOk),
    Effect.flatMap(
      HttpClientResponse.schemaBodyJson(Schema.Struct({ environmentId: Schema.String })),
    ),
    Effect.map((environment) => environment.environmentId),
    Effect.provide(FetchHttpClient.layer),
  );

export const test = base.extend<{ onicode: Onicode }>({
  onicode: async ({ app, screen }, use) => {
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
        // the bug. A narrow window collapses the sidebar to its toggle.
        await screen
          .getByRole("navigation", "Threads")
          .or(screen.getByRole("button", "Toggle main sidebar"))
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
