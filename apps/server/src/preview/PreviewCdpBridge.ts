// @effect-diagnostics nodeBuiltinImport:off - the loopback listener and its path secrets come from Node.
import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import { type PreviewAutomationStatus, PreviewTabId, type ThreadId } from "@onicode/contracts";
import { normalizePreviewUrl } from "@onicode/shared/preview";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as FiberSet from "effect/FiberSet";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import { HttpServerRequest, HttpServerResponse } from "effect/http";
import type * as Socket from "effect/socket/Socket";
import * as NodeCrypto from "node:crypto";
import * as NodeHttp from "node:http";

import type * as McpInvocationContext from "../mcp/McpInvocationContext.ts";
import * as PreviewAutomationBroker from "../mcp/PreviewAutomationBroker.ts";
import { requestUrl } from "../requestUrl.ts";
import {
  cdpErrorMessage,
  driverTargetId,
  isPanelUrl,
  routePageCommand,
} from "./previewCdpPolicy.ts";
import * as ServerBrowser from "./ServerBrowser.ts";

export interface PreviewCdpEndpoint {
  readonly host: string;
  readonly port: number;
  /** A secret path prefix: whoever knows it drives this thread's tabs. */
  readonly path: string;
}

export class PreviewCdpBridgeError extends Schema.TaggedError<PreviewCdpBridgeError>()(
  "PreviewCdpBridgeError",
  { cause: Schema.Defect() },
) {
  override get message(): string {
    return "The Browser panel's debug endpoint did not start.";
  }
}

/**
 * Serves a thread's Browser panel tabs as a Chrome debug endpoint (`/json`, `/json/version`
 * and one WebSocket per page), so a CDP driver such as obu drives the tabs the user watches.
 */
export class PreviewCdpBridge extends Context.Service<
  PreviewCdpBridge,
  {
    /** Stays the same for a thread until its provider session changes. */
    readonly endpoint: (
      scope: McpInvocationContext.McpThreadInvocationScope,
    ) => Effect.Effect<PreviewCdpEndpoint, PreviewCdpBridgeError>;
    readonly release: (threadId: ThreadId) => Effect.Effect<void>;
    /** Connects the tabs. Requests wait for it. */
    readonly bind: (backend: PreviewCdpBackend) => Effect.Effect<void>;
  }
>()("onicode/preview/PreviewCdpBridge") {}

export interface PreviewCdpBackend {
  readonly drivers: ServerBrowser.ServerBrowserDrivers["Service"];
  readonly broker: PreviewAutomationBroker.PreviewAutomationBroker["Service"];
}

class CommandRefused extends Schema.TaggedError<CommandRefused>()("PreviewCdpCommandRefused", {
  message: Schema.String,
}) {}

const CdpCommand = Schema.Struct({
  id: Schema.Number,
  method: Schema.String,
  params: Schema.optional(Schema.Unknown),
  sessionId: Schema.optional(Schema.String),
});
type CdpCommand = typeof CdpCommand.Type;
const decodeCommand = Schema.decodeUnknownOption(Schema.fromJsonString(CdpCommand));

interface BridgeSession {
  readonly scope: McpInvocationContext.McpThreadInvocationScope;
  readonly agentSessionId: string;
  readonly secret: string;
}

const HOST = "127.0.0.1";
const ENDPOINT_PATH = /^\/k\/([A-Za-z0-9_-]+)(\/.*)$/;
const BROWSER_SOCKET = "/devtools/browser/onicode";
const PAGE_SOCKET = "/devtools/page/";
const PANEL_URL_REQUIRED = "The Browser panel opens http and https pages only.";
const textDecoder = new TextDecoder();

const stringField = (value: unknown, field: string) =>
  typeof value === "object" && value !== null && field in value
    ? (value as Record<string, unknown>)[field]
    : undefined;

const notFound = HttpServerResponse.text("Not Found", { status: 404 });

const make = Effect.gen(function* () {
  // Provider sessions are prepared below the layer that owns the tabs, so the tabs arrive later.
  const bound = yield* Deferred.make<PreviewCdpBackend>();
  const backend = Deferred.await(bound);
  const serviceScope = yield* Effect.scope;
  const sessions = new Map<string, BridgeSession>();
  const secrets = new Map<ThreadId, string>();

  const targetsOf = (session: BridgeSession) =>
    backend.pipe(
      Effect.flatMap(({ drivers }) =>
        drivers.tabs({
          threadId: session.scope.thread.threadId,
          agentSessionId: session.agentSessionId,
        }),
      ),
      Effect.map((tabs) =>
        tabs.map((tab) => ({
          ...tab,
          targetId: driverTargetId(session.scope.thread.threadId, tab.tabId),
        })),
      ),
    );
  type Target = Effect.Success<ReturnType<typeof targetsOf>>[number];

  const targetInfo = (target: Target, attached: boolean) => ({
    targetId: target.targetId,
    type: "page",
    title: target.title,
    url: target.url,
    attached,
    canAccessOpener: false,
  });

  const createTarget = Effect.fn("PreviewCdpBridge.createTarget")(function* (
    session: BridgeSession,
    params: unknown,
  ) {
    const requested = stringField(params, "url");
    const blank = typeof requested !== "string" || requested === "" || requested === "about:blank";
    if (blank) {
      // A driver starts on a blank tab of its own; the one the agent already has is that tab.
      const own = (yield* targetsOf(session)).find((target) => target.own);
      if (own) return { targetId: own.targetId };
    }
    const url = blank
      ? undefined
      : yield* Effect.try({
          try: () => normalizePreviewUrl(requested),
          catch: () => new CommandRefused({ message: PANEL_URL_REQUIRED }),
        });
    const { broker } = yield* backend;
    const status = yield* broker
      .invoke<PreviewAutomationStatus>({
        scope: session.scope,
        operation: "open",
        input: { ...(url === undefined ? {} : { url }), reuseExistingTab: false },
      })
      .pipe(Effect.mapError((error) => new CommandRefused({ message: error.message })));
    if (status.tabId === null) {
      return yield* new CommandRefused({ message: "The Browser panel did not open a tab." });
    }
    return { targetId: driverTargetId(session.scope.thread.threadId, status.tabId) };
  });

  const closeTarget = Effect.fn("PreviewCdpBridge.closeTarget")(function* (
    session: BridgeSession,
    params: unknown,
  ) {
    const targetId = stringField(params, "targetId");
    const target = (yield* targetsOf(session)).find((entry) => entry.targetId === targetId);
    if (!target) return { success: false };
    const { broker } = yield* backend;
    yield* broker
      .invoke({
        scope: session.scope,
        operation: "close",
        input: {},
        tabId: PreviewTabId.make(target.tabId),
      })
      .pipe(Effect.mapError((error) => new CommandRefused({ message: error.message })));
    return { success: true };
  });

  const runTargetCommand = (
    session: BridgeSession,
    command: CdpCommand,
    attachedTargetId: string | undefined,
  ): Effect.Effect<unknown, CommandRefused> => {
    switch (command.method) {
      case "Target.createTarget":
        return createTarget(session, command.params);
      case "Target.closeTarget":
        return closeTarget(session, command.params);
      case "Target.getTargets":
        return targetsOf(session).pipe(
          Effect.map((targets) => ({
            targetInfos: targets.map((target) =>
              targetInfo(target, target.targetId === attachedTargetId),
            ),
          })),
        );
      default: {
        const targetId = stringField(command.params, "targetId") ?? attachedTargetId;
        return targetsOf(session).pipe(
          Effect.flatMap((targets) => {
            const target = targets.find((entry) => entry.targetId === targetId);
            return target
              ? Effect.succeed({ targetInfo: targetInfo(target, true) })
              : Effect.fail(new CommandRefused({ message: "No target with given id found" }));
          }),
        );
      }
    }
  };

  const runPageCommand = (
    session: BridgeSession,
    targetId: string,
    driver: ServerBrowser.ServerBrowserDriverSession,
    command: CdpCommand,
  ): Effect.Effect<unknown, CommandRefused> => {
    if (command.sessionId !== undefined) {
      return Effect.fail(
        new CommandRefused({ message: "Child sessions are not available in the Browser panel." }),
      );
    }
    switch (routePageCommand(command.method)) {
      case "acknowledge":
        return Effect.succeed({});
      case "refuse":
        return Effect.fail(
          new CommandRefused({
            message: `${command.method} is not available in the Browser panel.`,
          }),
        );
      case "target":
        return runTargetCommand(session, command, targetId);
      case "forward": {
        const url = stringField(command.params, "url");
        if (command.method === "Page.navigate" && !(typeof url === "string" && isPanelUrl(url))) {
          return Effect.fail(new CommandRefused({ message: PANEL_URL_REQUIRED }));
        }
        return Effect.tryPromise({
          try: () => driver.send(command.method, command.params ?? {}),
          catch: (cause) => new CommandRefused({ message: cdpErrorMessage(cause) }),
        });
      }
    }
  };

  const runBrowserCommand = (
    session: BridgeSession,
    command: CdpCommand,
  ): Effect.Effect<unknown, CommandRefused> => {
    if (command.method === "Browser.getVersion") {
      return Effect.succeed({
        protocolVersion: "1.3",
        product: "Onicode/Browser panel",
        revision: "",
        userAgent: "",
        jsVersion: "",
      });
    }
    return routePageCommand(command.method) === "target"
      ? runTargetCommand(session, command, undefined)
      : Effect.fail(
          new CommandRefused({
            message: `${command.method} is not available in the Browser panel.`,
          }),
        );
  };

  // The driver pipelines commands and one may wait on the page, so each runs in its own fiber.
  const serveSocket = Effect.fnUntraced(function* (
    socket: Socket.Socket,
    outbound: Queue.Queue<string>,
    run: (command: CdpCommand) => Effect.Effect<unknown, CommandRefused>,
    until: Effect.Effect<void>,
  ) {
    const writer = yield* socket.writer;
    const reader = yield* socket.reader;
    const running = yield* FiberSet.make<void>();
    const answer = (command: CdpCommand) =>
      run(command).pipe(
        Effect.matchCause({
          onSuccess: (result) => ({ id: command.id, result: result ?? {} }),
          onFailure: (cause) => ({
            id: command.id,
            error: {
              code: -32000,
              message: Option.match(Cause.findErrorOption(cause), {
                onNone: () => "The Browser panel could not run this command.",
                onSome: (error) => error.message,
              }),
            },
          }),
        }),
        Effect.flatMap((reply) => Queue.offer(outbound, JSON.stringify(reply))),
        Effect.asVoid,
      );
    const read = reader.pull.pipe(
      Effect.flatMap((frames) =>
        Effect.forEach(
          frames,
          (frame) => {
            const command = decodeCommand(
              typeof frame === "string" ? frame : textDecoder.decode(frame),
            );
            return Option.isNone(command)
              ? Effect.void
              : answer(command.value).pipe(FiberSet.run(running), Effect.asVoid);
          },
          { discard: true },
        ),
      ),
      Effect.forever,
    );
    const write = Stream.fromQueue(outbound).pipe(
      Stream.runForEach((message) => writer.write(message)),
    );
    // The first to end, by a closed socket too, ends the session.
    yield* read.pipe(Effect.raceFirst(write), Effect.raceFirst(until), Effect.ignore);
  });

  const servePage = Effect.fnUntraced(function* (
    session: BridgeSession,
    targetId: string,
    request: HttpServerRequest.HttpServerRequest,
  ) {
    const target = (yield* targetsOf(session)).find((entry) => entry.targetId === targetId);
    if (!target) return notFound;
    const outbound = yield* Queue.unbounded<string>();
    const { drivers } = yield* backend;
    const driver = yield* drivers
      .attach({
        threadId: session.scope.thread.threadId,
        tabId: target.tabId,
        agentSessionId: session.agentSessionId,
        onEvent: (method, params) => {
          Queue.offerUnsafe(outbound, JSON.stringify({ method, params }));
        },
      })
      .pipe(Effect.option);
    if (Option.isNone(driver)) return notFound;
    const socket = yield* request.upgrade;
    yield* serveSocket(
      socket,
      outbound,
      (command) => runPageCommand(session, targetId, driver.value, command),
      Effect.promise(() => driver.value.closed),
    );
    return HttpServerResponse.empty();
  }, Effect.scoped);

  const serveBrowser = Effect.fnUntraced(function* (
    session: BridgeSession,
    request: HttpServerRequest.HttpServerRequest,
  ) {
    const socket = yield* request.upgrade;
    yield* serveSocket(
      socket,
      yield* Queue.unbounded<string>(),
      (command) => runBrowserCommand(session, command),
      Effect.never,
    );
    return HttpServerResponse.empty();
  }, Effect.scoped);

  // Untraced: a driver polls `/json`, and each socket lives as long as its tab.
  const handle = Effect.fnUntraced(
    function* (port: number) {
      const request = yield* HttpServerRequest.HttpServerRequest;
      const match = Option.match(requestUrl(request), {
        onNone: () => null,
        onSome: (url) => ENDPOINT_PATH.exec(url.pathname),
      });
      const session = match ? sessions.get(match[1]!) : undefined;
      if (!match || !session) return notFound;
      const route = match[2]!;
      const socketBase = `ws://${HOST}:${port}/k/${session.secret}`;
      if (request.headers.upgrade?.toLowerCase() === "websocket") {
        if (route === BROWSER_SOCKET) return yield* serveBrowser(session, request);
        return route.startsWith(PAGE_SOCKET)
          ? yield* servePage(session, route.slice(PAGE_SOCKET.length), request)
          : notFound;
      }
      if (route === "/json/version") {
        return HttpServerResponse.jsonUnsafe({
          Browser: "Onicode/Browser panel",
          "Protocol-Version": "1.3",
          webSocketDebuggerUrl: `${socketBase}${BROWSER_SOCKET}`,
        });
      }
      if (route === "/json" || route === "/json/list") {
        return HttpServerResponse.jsonUnsafe(
          (yield* targetsOf(session)).map((target) => ({
            id: target.targetId,
            type: "page",
            title: target.title,
            url: target.url,
            webSocketDebuggerUrl: `${socketBase}${PAGE_SOCKET}${target.targetId}`,
          })),
        );
      }
      return notFound;
    },
    Effect.catchCause((cause) =>
      Effect.logWarning("The Browser panel's debug endpoint failed a request.", { cause }).pipe(
        Effect.as(HttpServerResponse.empty({ status: 500 })),
      ),
    ),
  );

  // Started by the first thread that needs it: most servers never run a driver in the panel.
  const listening = yield* Effect.cached(
    Effect.gen(function* () {
      const server = yield* NodeHttpServer.make(() => NodeHttp.createServer(), {
        host: HOST,
        port: 0,
      });
      const address = server.address;
      if (address._tag !== "InetAddressV4") return yield* Effect.die("Unexpected bridge address.");
      yield* server.serve(handle(address.port));
      return address.port;
    }).pipe(
      Scope.provide(serviceScope),
      Effect.mapError((cause) => new PreviewCdpBridgeError({ cause })),
    ),
  );

  return PreviewCdpBridge.of({
    endpoint: Effect.fn("PreviewCdpBridge.endpoint")(function* (scope) {
      const port = yield* listening;
      const threadId = scope.thread.threadId;
      const agentSessionId = PreviewAutomationBroker.hostAssignmentKey(scope);
      const current = sessions.get(secrets.get(threadId) ?? "");
      if (current?.agentSessionId === agentSessionId) {
        return { host: HOST, port, path: `/k/${current.secret}` };
      }
      if (current) sessions.delete(current.secret);
      const secret = NodeCrypto.randomBytes(24).toString("base64url");
      sessions.set(secret, { scope, agentSessionId, secret });
      secrets.set(threadId, secret);
      return { host: HOST, port, path: `/k/${secret}` };
    }),
    release: Effect.fn("PreviewCdpBridge.release")((threadId) =>
      Effect.sync(() => {
        const secret = secrets.get(threadId);
        if (secret === undefined) return;
        secrets.delete(threadId);
        sessions.delete(secret);
      }),
    ),
    bind: Effect.fn("PreviewCdpBridge.bind")(function* (value) {
      yield* Deferred.succeed(bound, value);
    }),
  });
});

export const layer = Layer.effect(PreviewCdpBridge, make);

/** Built where the Browser panel's tabs are; shares the bridge with `layer` by reference. */
export const layerBackend = Layer.effectDiscard(
  Effect.gen(function* () {
    const bridge = yield* PreviewCdpBridge;
    yield* bridge.bind({
      drivers: yield* ServerBrowser.ServerBrowserDrivers,
      broker: yield* PreviewAutomationBroker.PreviewAutomationBroker,
    });
  }),
);
