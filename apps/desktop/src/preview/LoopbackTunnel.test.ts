import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeWS from "@effect/platform-node/NodeWS";
import { EnvironmentId } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as NetService from "@onicode/shared/Net";
import { expect, it } from "@effect/vitest";
import * as NodeNet from "node:net";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Result from "effect/Result";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";

import * as ElectronApp from "../electron/ElectronApp.ts";
import * as ElectronWindow from "../electron/ElectronWindow.ts";
import * as LoopbackTunnel from "./LoopbackTunnel.ts";

const addressPort = (server: NodeNet.Server | NodeWS.WebSocketServer) =>
  (server.address() as NodeNet.AddressInfo).port;

const closeServer = (server: NodeNet.Server | NodeWS.WebSocketServer) =>
  Effect.promise(
    () =>
      new Promise<void>((resolve) => {
        if (server instanceof NodeWS.WebSocketServer) {
          for (const client of server.clients) client.terminate();
        }
        server.close(() => resolve());
      }),
  );

/** A local listener that must never see a connection: the reviewer's own localhost server. */
const listenDecoy = (host = "127.0.0.1") =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{ server: NodeNet.Server; connections: () => number }>((resolve, reject) => {
          let connections = 0;
          const server = NodeNet.createServer((socket) => {
            connections++;
            socket.destroy();
          });
          server.once("error", reject);
          server.listen(0, host, () => resolve({ server, connections: () => connections }));
        }),
    ),
    ({ server }) => closeServer(server),
  );

/**
 * Stands in for the environment's /api/preview/tunnel. Echoes each frame behind a 0xee byte, or
 * sends `reply.text` once what it received contains `reply.after`, then closes unless `keepOpen`.
 * With `keepOpen` it answers each further `after` again, with the next of `frames` when given.
 */
const listenEndpoint = (reply?: {
  readonly text: string;
  readonly after?: string;
  readonly keepOpen?: boolean;
  /** Each answer in turn, as the frames it is sent in. */
  readonly frames?: ReadonlyArray<ReadonlyArray<string>>;
}) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{
          server: NodeWS.WebSocketServer;
          requests: string[];
          received: string[];
          /** Settles when the first frame arrives. */
          messaged: Promise<void>;
        }>((resolve) => {
          const requests: string[] = [];
          const received: string[] = [];
          const { promise: messaged, resolve: onMessage } = Promise.withResolvers<void>();
          const server: NodeWS.WebSocketServer = new NodeWS.WebSocketServer(
            { port: 0, host: "127.0.0.1" },
            () => resolve({ server, requests, received, messaged }),
          );
          let replied = 0;
          server.on("connection", (socket: NodeWS.WebSocket, request: { url?: string }) => {
            requests.push(request.url ?? "");
            socket.on("message", (data: Buffer) => {
              received.push(data.toString("latin1"));
              onMessage();
              if (reply === undefined) {
                socket.send(Buffer.concat([Uint8Array.of(0xee), data]));
              } else if (
                (replied === 0 || reply.keepOpen) &&
                received.join("").split(reply.after ?? "").length - 1 > replied
              ) {
                const frames = reply.frames?.[replied] ?? [reply.text];
                replied++;
                frames.forEach((frame, index) =>
                  socket.send(Buffer.from(frame, "latin1"), () => {
                    if (index === frames.length - 1 && !reply.keepOpen) socket.close();
                  }),
                );
              }
            });
          });
        }),
    ),
    ({ server }) => closeServer(server),
  );

const unusedPort = Effect.promise(
  () =>
    new Promise<number>((resolve) => {
      const server = NodeNet.createServer();
      server.listen(0, "127.0.0.1", () => {
        const port = addressPort(server);
        server.close(() => resolve(port));
      });
    }),
);

type LoginListener = (
  event: { preventDefault: () => void },
  webContents: null,
  details: object,
  authInfo: Partial<Electron.AuthInfo>,
  callback: (username?: string, password?: string) => void,
) => void;

/** Answers a proxy challenge the way Chromium asks: undefined when main declines it. */
const challenge = (login: LoginListener, authInfo: Partial<Electron.AuthInfo>) => {
  let answer: string | undefined;
  login({ preventDefault: () => undefined }, null, {}, authInfo, (username, password) => {
    answer = `Basic ${Buffer.from(`${username}:${password}`).toString("base64")}`;
  });
  return answer;
};

/** The tunnel service with every access request answered with `tunnelUrl`, and main's login listener. */
const makeTunnel = (
  /** A function answers each access request with its current value. */
  tunnelUrl: string | null | (() => string | null),
  host: Layer.Layer<never> | Layer.Layer<ChildProcessSpawner.ChildProcessSpawner> = Layer.empty,
  { answerAccess = true }: { readonly answerAccess?: boolean } = {},
) =>
  Effect.gen(function* () {
    const accessRequests = yield* Queue.unbounded<string>();
    const window = {
      isDestroyed: () => false,
      webContents: {
        send: (_channel: string, request: { readonly requestId: string }) =>
          Queue.offerUnsafe(accessRequests, request.requestId),
      },
    };
    let login: LoginListener | undefined;
    const services = yield* Layer.build(
      LoopbackTunnel.layer.pipe(
        Layer.provide(
          Layer.mergeAll(
            Layer.mock(ElectronApp.ElectronApp, {
              on: (eventName, listener) =>
                Effect.sync(() => {
                  if (eventName === "login") login = listener as unknown as LoginListener;
                }),
            }),
            SecureRandom.layerWebCrypto,
            NodeServices.layer,
            NetService.layer,
            host,
            Layer.mock(ElectronWindow.ElectronWindow, {
              currentMainOrFirst: Effect.succeedSome(window as unknown as Electron.BrowserWindow),
            }),
          ),
        ),
      ),
    );
    const tunnel = Context.get(services, LoopbackTunnel.LoopbackTunnel);
    if (answerAccess) {
      yield* Queue.take(accessRequests).pipe(
        Effect.flatMap((requestId) =>
          tunnel.resolveAccess({
            requestId,
            url: typeof tunnelUrl === "function" ? tunnelUrl() : tunnelUrl,
          }),
        ),
        Effect.forever,
        Effect.forkScoped,
      );
    }
    return { tunnel, login: login!, accessRequests };
  });

/**
 * Routes a stand-in session through the tunnel; returns its proxy port, Chromium's credentials,
 * and the session's request filter.
 */
const routeThroughTunnel = (tunnelUrl: string | null) =>
  Effect.gen(function* () {
    const { tunnel, login } = yield* makeTunnel(tunnelUrl);
    let proxyRules = "";
    let beforeRequest:
      | {
          readonly urls: ReadonlyArray<string>;
          readonly listener: (
            details: Electron.OnBeforeRequestListenerDetails,
            callback: (response: Electron.CallbackResponse) => void,
          ) => void;
        }
      | undefined;
    const session = {
      setProxy: async (config: Electron.ProxyConfig) => {
        proxyRules = config.proxyRules ?? "";
      },
      webRequest: {
        onBeforeRequest: (
          filter: Electron.WebRequestFilter,
          listener: NonNullable<typeof beforeRequest>["listener"],
        ) => {
          beforeRequest = { urls: filter.urls, listener };
        },
      },
    };
    yield* tunnel.route({
      environmentId: "remote",
      session: session as unknown as Electron.Session,
    });
    const proxyPort = Number(/^http:\/\/127\.0\.0\.1:(\d+)$/.exec(proxyRules)![1]);
    const authorization = challenge(login, {
      isProxy: true,
      scheme: "basic",
      host: "127.0.0.1",
      port: proxyPort,
    });
    return { proxyPort, authorization: authorization!, login, beforeRequest: beforeRequest! };
  });

/** Sends `request` to the proxy; resolves with the response head, or everything read until close. */
const sendToProxy = (proxyPort: number, request: Uint8Array | string) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{ socket: NodeNet.Socket; head: string }>((resolve, reject) => {
          const socket = NodeNet.connect(proxyPort, "127.0.0.1", () => socket.write(request));
          let received = Buffer.alloc(0);
          const onData = (chunk: Buffer) => {
            received = Buffer.concat([received, chunk]);
            const end = received.indexOf("\r\n\r\n");
            if (end < 0) return;
            socket.off("data", onData).off("close", onClose);
            resolve({ socket, head: received.subarray(0, end).toString("latin1") });
          };
          const onClose = () => resolve({ socket, head: received.toString("latin1") });
          socket.on("data", onData).on("close", onClose).on("error", reject);
        }),
    ),
    ({ socket }) => Effect.sync(() => socket.destroy()),
  );

const connectRequest = (authority: string, authorization?: string) =>
  `CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n` +
  (authorization === undefined ? "" : `Proxy-Authorization: ${authorization}\r\n`) +
  "Proxy-Connection: keep-alive\r\n\r\n";

const exchange = (socket: NodeNet.Socket, bytes: Uint8Array) =>
  Effect.promise(
    () =>
      new Promise<number[]>((resolve) => {
        socket.once("data", (chunk: Buffer) => resolve([...chunk]));
        socket.write(bytes);
      }),
  );

it.effect.each([
  { authority: "localhost", host: "" },
  { authority: "localhost.", host: "" },
  { authority: "[::1]", host: "&host=%3A%3A1" },
  { authority: "127.0.0.1", host: "&host=127.0.0.1" },
  { authority: "[::ffff:127.0.0.1]", host: "&host=127.0.0.1" },
  { authority: "[::ffff:7f00:1]", host: "&host=127.0.0.1" },
])(
  "carries a CONNECT to $authority over the tunnel WebSocket, not to local listeners",
  ({ authority, host }) =>
    Effect.gen(function* () {
      const decoy = yield* listenDecoy();
      const endpoint = yield* listenEndpoint();
      const { proxyPort, authorization } = yield* routeThroughTunnel(
        `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel?wsTicket=t`,
      );
      const decoyPort = addressPort(decoy.server);
      const { socket, head } = yield* sendToProxy(
        proxyPort,
        connectRequest(`${authority}:${decoyPort}`, authorization),
      );
      expect(head).toBe("HTTP/1.1 200 Connection Established");
      expect(yield* exchange(socket, Uint8Array.of(1, 2, 3))).toEqual([0xee, 1, 2, 3]);
      // The tunnel reaches the address the page named on the environment, never its `localhost`.
      expect(endpoint.requests).toEqual([
        `/api/preview/tunnel?wsTicket=t&port=${decoyPort}${host}`,
      ]);
      expect(decoy.connections()).toBe(0);
    }),
);

it.effect("keeps requests the proxy bypass would send to this machine in the tunnel", () =>
  Effect.gen(function* () {
    const { beforeRequest } = yield* routeThroughTunnel(null);
    // Chromium canonicalizes `[::ffff:127.0.0.1]` to this form before matching filters.
    expect(beforeRequest.urls).toContain("http://[::ffff:7f00:1]/*");
    expect(beforeRequest.urls).toContain("ws://[::ffff:7f00:1]/*");
    const answer = (url: string) => {
      let response: Electron.CallbackResponse | undefined;
      beforeRequest.listener({ url } as Electron.OnBeforeRequestListenerDetails, (value) => {
        response = value;
      });
      return response;
    };
    expect(answer("http://[::ffff:7f00:1]:3200/app?q=1")).toEqual({
      redirectURL: "http://127.0.0.1:3200/app?q=1",
    });
    expect(answer("http://0.0.0.0:3200/")).toEqual({ redirectURL: "http://localhost:3200/" });
    expect(answer("http://[::ffff:0:0]:3200/")).toEqual({ redirectURL: "http://localhost:3200/" });
    expect(answer("ws://[::ffff:7f00:1]:3200/hmr")).toEqual({ cancel: true });
  }),
);

it.effect.each([
  {
    case: "a keep-alive response",
    reply:
      "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\n\r\nok",
    head: "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5",
  },
  {
    // Chromium would answer it with this run's credentials and send them on.
    case: "the environment's own proxy challenge",
    reply:
      'HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm="x"\r\nContent-Length: 0\r\n\r\n',
    head: "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0",
  },
  {
    case: "a proxy challenge without a reason phrase",
    reply: 'HTTP/1.1 407\r\nProxy-Authenticate: Basic realm="x"\r\nContent-Length: 0\r\n\r\n',
    head: "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0",
  },
  {
    // A bare close would make Chromium send the request, even a POST, again.
    case: "a response cut off inside its head",
    reply: "HTTP/1.1 200 OK\r\nContent-",
    head: "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close",
  },
  {
    // Passed on unread it would desynchronise later responses; a bare close would make Chromium
    // send the request again.
    case: "a response head over the size limit",
    reply: `HTTP/1.1 200 OK\r\nX-Padding: ${"a".repeat(300_000)}\r\nContent-Length: 0\r\n\r\n`,
    head: "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close",
  },
])(
  "forwards a plain http request in origin form, answering $case",
  ({ reply, head: expectedHead }) =>
    Effect.gen(function* () {
      const endpoint = yield* listenEndpoint({ text: reply });
      const { proxyPort, authorization } = yield* routeThroughTunnel(
        `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel?wsTicket=t`,
      );
      const { head } = yield* sendToProxy(
        proxyPort,
        "GET http://localhost:5173/app?r='y'&x=1 HTTP/1.1\r\nHost: localhost:5173\r\n" +
          `Proxy-Connection: keep-alive\r\nProxy-Authorization: ${authorization}\r\n` +
          "Accept: */*\r\n\r\n",
      );
      // The credentials stay on this machine.
      expect(endpoint.received.join("")).toBe(
        "GET /app?r='y'&x=1 HTTP/1.1\r\nHost: localhost:5173\r\nAccept: */*\r\n\r\n",
      );
      expect(endpoint.requests).toEqual(["/api/preview/tunnel?wsTicket=t&port=5173"]);
      expect(head).toBe(expectedHead);
    }),
);

/** Sends `request` to the proxy and resolves with everything read until `until` appears. */
const readFromProxy = (proxyPort: number, request: string, until: string) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{ socket: NodeNet.Socket; received: string }>((resolve, reject) => {
          const socket = NodeNet.connect(proxyPort, "127.0.0.1", () => socket.write(request));
          let received = "";
          socket.on("data", (chunk: Buffer) => {
            received += chunk.toString("latin1");
            if (received.includes(until)) resolve({ socket, received });
          });
          socket.on("error", reject);
        }),
    ),
    ({ socket }) => Effect.sync(() => socket.destroy()),
  );

/** One client connection to the proxy; `send` resolves once a reply after it contains `until`. */
const openProxyConnection = (proxyPort: number) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<NodeNet.Socket>((resolve, reject) => {
          const socket = NodeNet.connect(proxyPort, "127.0.0.1", () => resolve(socket));
          socket.on("error", reject);
        }),
    ),
    (socket) => Effect.sync(() => socket.destroy()),
  ).pipe(
    Effect.map((socket) => {
      let received = "";
      const closed = new Promise<string>((resolve) => socket.on("close", () => resolve(received)));
      socket.on("data", (chunk: Buffer) => (received += chunk.toString("latin1")));
      const send = (request: string, until: string) =>
        Effect.promise(
          () =>
            new Promise<string>((resolve) => {
              const from = received.length;
              const check = () => {
                if (received.indexOf(until, from) < 0) return;
                socket.off("data", check);
                resolve(received.slice(from));
              };
              socket.on("data", check);
              socket.write(request);
            }),
        );
      return {
        send,
        closed: (request: string) =>
          Effect.promise(() => {
            const from = received.length;
            socket.write(request);
            return closed.then((all) => all.slice(from));
          }),
      };
    }),
  );

it.effect("keeps a connection to one server for Chromium's next request", () =>
  Effect.gen(function* () {
    // A dev server's page loads hundreds of modules; a tunnel each would cost round trips.
    const endpoint = yield* listenEndpoint({
      text: "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
      after: "\r\n\r\n",
      keepOpen: true,
      // A body after its head, then a later response whose head must still be read.
      frames: [
        ["HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n", "ok"],
        ['HTTP/1.1 407 Nope\r\nProxy-Authenticate: Basic realm="x"\r\nContent-Length: 2\r\n\r\nno'],
      ],
    });
    const { proxyPort, authorization } = yield* routeThroughTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
    );
    const get = (authority: string, path: string) =>
      `GET http://${authority}${path} HTTP/1.1\r\nHost: ${authority}\r\n` +
      `Proxy-Authorization: ${authorization}\r\n\r\n`;
    const connection = yield* openProxyConnection(proxyPort);
    expect(yield* connection.send(get("localhost:5173", "/first"), "\r\n\r\nok")).toBe(
      "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
    );
    expect(yield* connection.send(get("localhost:5173", "/second"), "\r\n\r\nno")).toBe(
      "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 2\r\n\r\nno",
    );
    // Each request on the connection is checked again.
    const anonymous = "GET http://localhost:5173/third HTTP/1.1\r\nHost: localhost:5173\r\n\r\n";
    expect(yield* connection.closed(anonymous)).toMatch(/^HTTP\/1\.1 407 /);
    // Another server needs its own tunnel; Chromium retries on a new connection.
    const other = yield* openProxyConnection(proxyPort);
    yield* other.send(get("localhost:5173", "/fourth"), "\r\n\r\nok");
    yield* other.closed(get("localhost:5174", "/fifth"));
    expect(endpoint.requests).toEqual([
      "/api/preview/tunnel?port=5173",
      "/api/preview/tunnel?port=5173",
    ]);
    expect(endpoint.received.join("")).toBe(
      "GET /first HTTP/1.1\r\nHost: localhost:5173\r\n\r\n" +
        "GET /second HTTP/1.1\r\nHost: localhost:5173\r\n\r\n" +
        "GET /fourth HTTP/1.1\r\nHost: localhost:5173\r\n\r\n",
    );
  }),
);

it.effect("streams a request body to the environment as it arrives", () =>
  Effect.gen(function* () {
    // A large upload must not sit in desktop main until its last byte.
    const endpoint = yield* listenEndpoint({
      text: "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
      after: "abc",
    });
    const { proxyPort, authorization } = yield* routeThroughTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
    );
    const { received } = yield* readFromProxy(
      proxyPort,
      "POST http://localhost:5173/upload HTTP/1.1\r\nHost: localhost:5173\r\n" +
        `Proxy-Authorization: ${authorization}\r\nContent-Length: 6\r\n\r\nabc`,
      "\r\n\r\nok",
    );
    expect(received).toBe("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
  }),
);

it.effect.each([
  { case: "a client without credentials", request: "connect-anonymous", reply: "407" },
  { case: "a client with other credentials", request: "connect-wrong", reply: "407" },
  { case: "a SOCKS client", request: "socks", reply: "400" },
  { case: "a non-loopback destination", request: "connect-lan", reply: "403" },
  { case: "a request head over the size limit", request: "oversized", reply: "400" },
  { case: "a chunked request body", request: "chunked", reply: "501" },
  { case: "no tunnel URL", request: "connect", tunnel: "none", reply: "502" },
  {
    case: "an unreachable tunnel endpoint",
    request: "connect",
    tunnel: "unreachable",
    reply: "502",
  },
  { case: "a forwarded request with no tunnel URL", request: "forward", tunnel: "none", reply: "" },
] as const)("refuses $case without dialling locally", ({ request, tunnel, reply }) =>
  Effect.gen(function* () {
    const decoy = yield* listenDecoy();
    const endpoint = yield* listenEndpoint();
    const endpointPort =
      tunnel === "unreachable" ? yield* unusedPort : addressPort(endpoint.server);
    const { proxyPort, authorization } = yield* routeThroughTunnel(
      tunnel === "none" ? null : `ws://127.0.0.1:${endpointPort}/api/preview/tunnel`,
    );
    const decoyPort = addressPort(decoy.server);
    const bytes = {
      "connect-anonymous": connectRequest(`localhost:${decoyPort}`),
      "connect-wrong": connectRequest(
        `localhost:${decoyPort}`,
        `Basic ${Buffer.from("onicode:guess").toString("base64")}`,
      ),
      // Any local process can speak SOCKS5 to a port it finds open.
      socks: Uint8Array.of(5, 1, 0),
      "connect-lan": connectRequest("192.168.1.20:80", authorization),
      oversized: connectRequest(`localhost:${decoyPort}`, authorization).replace(
        "\r\n\r\n",
        `\r\nX-Padding: ${"a".repeat(300_000)}\r\n\r\n`,
      ),
      chunked:
        `POST http://localhost:${decoyPort}/ HTTP/1.1\r\nHost: localhost:${decoyPort}\r\n` +
        `Proxy-Authorization: ${authorization}\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n`,
      connect: connectRequest(`localhost:${decoyPort}`, authorization),
      forward:
        `GET http://localhost:${decoyPort}/ HTTP/1.1\r\nHost: localhost:${decoyPort}\r\n` +
        `Proxy-Authorization: ${authorization}\r\n\r\n`,
    }[request];
    const { head } = yield* sendToProxy(proxyPort, bytes);
    expect(head.split(" ")[1] ?? "").toBe(reply);
    if (reply === "407")
      expect(head).toContain('Proxy-Authenticate: Basic realm="Onicode preview"');
    expect(endpoint.requests).toEqual([]);
    expect(decoy.connections()).toBe(0);
  }),
);

it.effect("answers only proxy challenges from its own listeners", () =>
  Effect.gen(function* () {
    const { proxyPort, login } = yield* routeThroughTunnel(null);
    const base = { isProxy: true, scheme: "basic", host: "127.0.0.1", port: proxyPort };
    expect(challenge(login, { ...base, isProxy: false })).toBeUndefined();
    expect(challenge(login, { ...base, port: proxyPort + 1 })).toBeUndefined();
    expect(challenge(login, { ...base, host: "proxy.example.com" })).toBeUndefined();
  }),
);

it.effect("connects straight to the environment's own forwarded origin port", () =>
  Effect.gen(function* () {
    // Over SSH the origin is a local forward, and signed file previews load from it.
    const received: number[] = [];
    const origin = yield* Effect.acquireRelease(
      Effect.promise(
        () =>
          new Promise<NodeNet.Server>((resolve) => {
            const server = NodeNet.createServer((socket) =>
              socket.on("data", (chunk: Buffer) => {
                received.push(...chunk);
                socket.write(chunk);
              }),
            );
            server.listen(0, "127.0.0.1", () => resolve(server));
          }),
      ),
      closeServer,
    );
    const port = addressPort(origin);
    const { proxyPort, authorization } = yield* routeThroughTunnel(
      `ws://127.0.0.1:${port}/api/preview/tunnel`,
    );
    const { socket, head } = yield* sendToProxy(
      proxyPort,
      connectRequest(`127.0.0.1:${port}`, authorization),
    );
    expect(head).toBe("HTTP/1.1 200 Connection Established");
    expect(yield* exchange(socket, Uint8Array.of(7, 8))).toEqual([7, 8]);
    expect(received).toEqual([7, 8]);
    // `localhost` names the forward too; another address at that port is a server there.
    const named = yield* sendToProxy(proxyPort, connectRequest(`localhost:${port}`, authorization));
    expect(named.head).toBe("HTTP/1.1 200 Connection Established");
    const other = yield* sendToProxy(proxyPort, connectRequest(`[::1]:${port}`, authorization));
    expect(other.head).toBe("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close");
  }),
);

/** Connects to a local port, sends `bytes`, and resolves with the first chunk back. */
const exchangeWithPort = (port: number, bytes: Uint8Array, host = "127.0.0.1") =>
  Effect.promise(
    () =>
      new Promise<number[]>((resolve, reject) => {
        const socket = NodeNet.connect(port, host, () => socket.write(bytes));
        socket.once("data", (chunk: Buffer) => {
          resolve([...chunk]);
          socket.destroy();
        });
        socket.on("error", reject);
      }),
  );

const refusesConnections = (port: number) =>
  Effect.promise(
    () =>
      new Promise<boolean>((resolve) => {
        const socket = NodeNet.connect(port, "127.0.0.1", () => {
          socket.destroy();
          resolve(false);
        });
        socket.on("error", () => resolve(true));
      }),
  );

it.effect("forwards a port over the tunnel on the same local port until stopped", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint();
    const remotePort = yield* unusedPort;
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel?wsTicket=t`,
    );
    const announced: number[][] = [];
    yield* tunnel.subscribeForwards((forwards) =>
      Effect.sync(() => announced.push(forwards.map((forward) => forward.localPort))),
    );
    const environmentId = EnvironmentId.make("remote");
    const forward = yield* tunnel.forward({ environmentId, remotePort });
    expect(forward).toEqual({ environmentId, remotePort, localPort: remotePort });

    expect(yield* exchangeWithPort(remotePort, Uint8Array.of(4, 5))).toEqual([0xee, 4, 5]);
    // The first request is the same-machine probe.
    expect(endpoint.requests.at(-1)).toBe(`/api/preview/tunnel?wsTicket=t&port=${remotePort}`);

    yield* tunnel.stopForward({ environmentId, remotePort });
    expect(yield* refusesConnections(remotePort)).toBe(true);
    expect(yield* tunnel.forwards).toEqual([]);
    expect(announced).toEqual([[remotePort], []]);
  }),
);

/** Connects to a local port; true when the connection closes before any byte comes back. */
const closedWithoutReply = (port: number, host = "127.0.0.1") =>
  Effect.promise(
    () =>
      new Promise<boolean>((resolve) => {
        const socket = NodeNet.connect(port, host);
        socket.on("error", () => undefined);
        socket.on("close", () => resolve(true));
        socket.on("data", () => {
          resolve(false);
          socket.destroy();
        });
        socket.write(Uint8Array.of(3));
      }),
  );

it.effect("closes another OS user's connection to a forward without tunnelling it", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint();
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
      // This process owns the client, so a different user id here makes it another user's.
      Layer.mergeAll(
        Layer.succeed(HostProcess.Platform, "linux"),
        Layer.succeed(HostProcess.UserId, (process.getuid?.() ?? 0) + 1),
      ),
    );
    const forward = yield* tunnel.forward({
      environmentId: EnvironmentId.make("remote"),
      remotePort: yield* unusedPort,
    });
    const probes = endpoint.requests.length;
    expect(yield* closedWithoutReply(forward.localPort)).toBe(true);
    expect(endpoint.requests.length).toBe(probes);
  }),
);

/** Stands in for the system's own tools: each command prints what `print` returns for it. */
const systemTools = (print: (command: string, args: ReadonlyArray<string>) => string) =>
  Layer.succeed(
    ChildProcessSpawner.ChildProcessSpawner,
    ChildProcessSpawner.make((command) =>
      Effect.sync(() => {
        const { command: name, args } = command as unknown as {
          readonly command: string;
          readonly args: ReadonlyArray<string>;
        };
        return ChildProcessSpawner.makeHandle({
          pid: ChildProcessSpawner.ProcessId(1),
          exitCode: Effect.succeed(ChildProcessSpawner.ExitCode(0)),
          isRunning: Effect.succeed(false),
          kill: () => Effect.void,
          unref: Effect.succeed(Effect.void),
          stdin: Sink.drain,
          stdout: Stream.make(new TextEncoder().encode(print(name, args))),
          stderr: Stream.empty,
          all: Stream.empty,
          getInputFd: () => Sink.drain,
          getOutputFd: () => Stream.empty,
        });
      }),
    ),
  );

// A container without IPv6, or a sandbox that lists ::1 yet refuses the bind, has none to use.
const hasIpv6Loopback = await new Promise<boolean>((resolve) => {
  const server = NodeNet.createServer();
  server.once("error", () => resolve(false));
  server.listen(0, "::1", () => server.close(() => resolve(true)));
});

const ownUserId = process.getuid?.() ?? 501;

it.effect.each(
  [
    { platform: "darwin", client: "own", host: "127.0.0.1", end: "127.0.0.1", served: true },
    // lsof takes and prints IPv6 in brackets.
    { platform: "darwin", client: "own", host: "::1", end: "[::1]", served: true },
    {
      platform: "darwin",
      client: "another user's",
      host: "127.0.0.1",
      end: "127.0.0.1",
      served: false,
    },
    { platform: "darwin", client: "unlisted", host: "127.0.0.1", end: "127.0.0.1", served: false },
    { platform: "win32", client: "unlisted", host: "127.0.0.1", end: "127.0.0.1", served: true },
  ].filter(({ host }) => !host.includes(":") || hasIpv6Loopback),
)(
  "on $platform serves a forward's $client client on $host: $served",
  ({ platform, client, host, end, served }) =>
    Effect.gen(function* () {
      const endpoint = yield* listenEndpoint();
      const commands: Array<string> = [];
      let port = 0;
      const lsof = (args: ReadonlyArray<string>) => {
        const clientEnd = /^-iTCP@(.+)$/.exec(args[1] ?? "")?.[1] ?? "";
        // lsof prints nothing for an address it does not accept.
        if (!clientEnd.startsWith(`${end}:`)) return "";
        // The listener's accepted socket is this app's own and is always listed.
        const accepted = `p100\nu${ownUserId}\nf30\nn${end}:${port}->${clientEnd}\n`;
        if (client === "unlisted") return accepted;
        const owner = client === "own" ? ownUserId : ownUserId + 1;
        return `${accepted}p200\nu${owner}\nf12\nn${clientEnd}->${end}:${port}\n`;
      };
      const { tunnel } = yield* makeTunnel(
        `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
        Layer.mergeAll(
          Layer.succeed(HostProcess.Platform, platform as NodeJS.Platform),
          Layer.succeed(HostProcess.UserId, platform === "win32" ? undefined : ownUserId),
          systemTools((command, args) => {
            commands.push(command);
            return command === "/usr/sbin/lsof" ? lsof(args) : "";
          }),
        ),
      );
      const forward = yield* tunnel.forward({
        environmentId: EnvironmentId.make("remote"),
        remotePort: yield* unusedPort,
      });
      port = forward.localPort;
      const probes = endpoint.requests.length;
      expect(yield* closedWithoutReply(port, host)).toBe(!served);
      expect(endpoint.requests.length).toBe(probes + (served ? 1 : 0));
      expect(commands).toEqual(platform === "darwin" ? ["/usr/sbin/lsof"] : []);
    }),
);

it.effect.each(
  [
    { case: "an IPv4 server", host: "127.0.0.1" },
    // `localhost` may resolve to it first, and a forward on 127.0.0.1 alone would leave it there.
    { case: "an IPv6 server", host: "::1" },
    // Node's default listen; Vite's on macOS is ::1.
    { case: "a dual-stack server", host: "::" },
    // `vite --host`, Rails -b 0.0.0.0, Docker ports.
    { case: "an IPv4 wildcard server", host: "0.0.0.0" },
  ].filter(({ host }) => !host.includes(":") || hasIpv6Loopback),
)("moves a forward to a free port when $case takes its own here", ({ host }) =>
  Effect.gen(function* () {
    const decoy = yield* listenDecoy(host);
    const endpoint = yield* listenEndpoint();
    const remotePort = addressPort(decoy.server);
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
    );
    const forward = yield* tunnel.forward({
      environmentId: EnvironmentId.make("remote"),
      remotePort,
    });
    expect(forward.localPort).not.toBe(remotePort);

    // `localhost` may resolve to either family, so both reach the forward.
    for (const client of hasIpv6Loopback ? ["127.0.0.1", "::1"] : ["127.0.0.1"]) {
      expect(yield* exchangeWithPort(forward.localPort, Uint8Array.of(6), client)).toEqual([
        0xee, 6,
      ]);
    }
    expect(endpoint.requests.at(-1)).toBe(`/api/preview/tunnel?port=${remotePort}`);
    expect(decoy.connections()).toBe(0);
  }),
);

it.effect("forwards the port of the environment's own SSH forward over the tunnel", () =>
  Effect.gen(function* () {
    // Over SSH the tunnel endpoint is a local port; that port number there is another server.
    const endpoint = yield* listenEndpoint();
    const endpointPort = addressPort(endpoint.server);
    const { tunnel } = yield* makeTunnel(`ws://127.0.0.1:${endpointPort}/api/preview/tunnel`);
    const forward = yield* tunnel.forward({
      environmentId: EnvironmentId.make("remote"),
      remotePort: endpointPort,
    });
    expect(yield* exchangeWithPort(forward.localPort, Uint8Array.of(5))).toEqual([0xee, 5]);
    expect(endpoint.requests.at(-1)).toBe(`/api/preview/tunnel?port=${endpointPort}`);
  }),
);

it.effect("stops a forward while another waits on its environment's answer", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint();
    // Takes the same-machine probe and never answers it.
    const silent = yield* listenEndpoint({ text: "", after: "never sent" });
    let access = `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`;
    const { tunnel } = yield* makeTunnel(() => access);
    const environmentId = EnvironmentId.make("remote");
    const remotePort = yield* unusedPort;
    yield* tunnel.forward({ environmentId, remotePort });

    access = `ws://127.0.0.1:${addressPort(silent.server)}/api/preview/tunnel`;
    yield* tunnel.forward({ environmentId, remotePort: yield* unusedPort }).pipe(Effect.forkScoped);
    yield* Effect.promise(() => silent.messaged);
    yield* tunnel.stopForward({ environmentId, remotePort });
    expect(yield* refusesConnections(remotePort)).toBe(true);
  }),
);

it.effect("fails a forward stopped while it was starting, though a later start bound it", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint();
    const url = `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`;
    const { tunnel, accessRequests } = yield* makeTunnel(url, Layer.empty, {
      answerAccess: false,
    });
    const environmentId = EnvironmentId.make("remote");
    const remotePort = yield* unusedPort;
    const starting = yield* tunnel
      .forward({ environmentId, remotePort })
      .pipe(Effect.flip, Effect.forkScoped);
    const requestId = yield* Queue.take(accessRequests);
    yield* tunnel.stopForward({ environmentId, remotePort });
    const replacing = yield* tunnel.forward({ environmentId, remotePort }).pipe(Effect.forkScoped);
    yield* tunnel.resolveAccess({ requestId: yield* Queue.take(accessRequests), url });
    const replacement = yield* Fiber.join(replacing);
    yield* tunnel.resolveAccess({ requestId, url });
    expect((yield* Fiber.join(starting))._tag).toBe("LoopbackTunnelForwardError");
    expect(yield* tunnel.forwards).toEqual([replacement]);
  }),
);

/** An environment on this machine: each tunnel connection dials the requested local port. */
const listenSameMachineEndpoint = Effect.acquireRelease(
  Effect.promise(
    () =>
      new Promise<NodeWS.WebSocketServer>((resolve) => {
        const server: NodeWS.WebSocketServer = new NodeWS.WebSocketServer(
          { port: 0, host: "127.0.0.1" },
          () => resolve(server),
        );
        server.on("connection", (socket: NodeWS.WebSocket, request: { url?: string }) => {
          const port = Number(new URL(request.url ?? "", "ws://x").searchParams.get("port"));
          const local = NodeNet.connect(port, "127.0.0.1");
          local.on("data", (chunk) => socket.send(chunk));
          local.on("error", () => socket.close());
          socket.on("message", (data: Buffer) => local.write(data));
          socket.on("close", () => local.destroy());
        });
      }),
  ),
  closeServer,
);

it.effect("refuses to forward from an environment that shares this machine's loopback", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenSameMachineEndpoint;
    const remotePort = yield* unusedPort;
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint)}/api/preview/tunnel`,
    );
    const error = yield* tunnel
      .forward({ environmentId: EnvironmentId.make("remote"), remotePort })
      .pipe(Effect.flip);
    expect(error._tag).toBe("LoopbackTunnelSameMachineError");
    expect(yield* refusesConnections(remotePort)).toBe(true);
    expect(yield* tunnel.forwards).toEqual([]);
  }),
);

it.effect(
  "forwards nothing while the environment is out of reach, then forwards once it is back",
  () =>
    Effect.gen(function* () {
      const endpoint = yield* listenEndpoint();
      const remotePort = yield* unusedPort;
      let access: string | null = null;
      const { tunnel } = yield* makeTunnel(() => access);
      const environmentId = EnvironmentId.make("remote");

      const unavailable = yield* tunnel.forward({ environmentId, remotePort }).pipe(Effect.flip);
      expect(unavailable._tag).toBe("LoopbackTunnelUnavailableError");
      expect(yield* refusesConnections(remotePort)).toBe(true);
      expect(yield* tunnel.forwards).toEqual([]);

      access = `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`;
      const forward = yield* tunnel.forward({ environmentId, remotePort });
      expect(yield* exchangeWithPort(forward.localPort, Uint8Array.of(9))).toEqual([0xee, 9]);
    }),
);

/** The environment's tunnel route refusing every port with a 502, as for a port nothing listens on. */
const listenRefusingEndpoint = (headers: Record<string, string>) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<NodeWS.WebSocketServer>((resolve) => {
          const server: NodeWS.WebSocketServer = new NodeWS.WebSocketServer(
            {
              port: 0,
              host: "127.0.0.1",
              verifyClient: (_info, done) => done(false, 502, "Bad Gateway", headers),
            },
            () => resolve(server),
          );
        }),
    ),
    closeServer,
  );

it.effect.each([
  { case: "the environment reports nothing on the probe port", refused: true },
  { case: "the tunnel fails for another reason", refused: false },
])("decides a forward's machine only on proof: $case", ({ refused }) =>
  Effect.gen(function* () {
    const endpoint = yield* listenRefusingEndpoint(
      refused ? { "x-oni-tunnel-port-refused": "1" } : {},
    );
    const remotePort = yield* unusedPort;
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint)}/api/preview/tunnel`,
    );
    const result = yield* tunnel
      .forward({ environmentId: EnvironmentId.make("remote"), remotePort })
      .pipe(Effect.result);
    if (refused) {
      expect(Result.isSuccess(result)).toBe(true);
    } else {
      expect(Result.isFailure(result) && result.failure._tag).toBe(
        "LoopbackTunnelUnavailableError",
      );
      expect(yield* refusesConnections(remotePort)).toBe(true);
    }
  }),
);

it.effect("closes connections beyond a forward's limit without tunnelling them", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint();
    const { tunnel } = yield* makeTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel`,
    );
    const forward = yield* tunnel.forward({
      environmentId: EnvironmentId.make("remote"),
      remotePort: yield* unusedPort,
    });
    const open = () =>
      Effect.acquireRelease(
        Effect.promise(
          () =>
            new Promise<NodeNet.Socket>((resolve) => {
              const socket = NodeNet.connect(forward.localPort, "127.0.0.1", () => {
                socket.once("data", () => resolve(socket));
                socket.write(Uint8Array.of(1));
              });
            }),
        ),
        (socket) => Effect.sync(() => socket.destroy()),
      );
    yield* Effect.forEach(Array.from({ length: 64 }), open, { concurrency: "unbounded" });
    const tunnelled = endpoint.requests.length;
    const closed = yield* Effect.promise(
      () =>
        new Promise<boolean>((resolve) => {
          const socket = NodeNet.connect(forward.localPort, "127.0.0.1");
          // macOS resets a socket closed with the write unread; `close` still follows.
          socket.on("error", () => undefined);
          socket.on("close", () => resolve(true));
          socket.on("data", () => resolve(false));
          socket.write(Uint8Array.of(2));
        }),
    );
    expect(closed).toBe(true);
    expect(endpoint.requests.length).toBe(tunnelled);
  }),
);
