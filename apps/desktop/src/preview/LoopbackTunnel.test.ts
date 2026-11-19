import * as NodeWS from "@effect/platform-node/NodeWS";
import { expect, it } from "@effect/vitest";
import * as NodeNet from "node:net";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";

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
const listenDecoy = Effect.acquireRelease(
  Effect.promise(
    () =>
      new Promise<{ server: NodeNet.Server; connections: () => number }>((resolve) => {
        let connections = 0;
        const server = NodeNet.createServer((socket) => {
          connections++;
          socket.destroy();
        });
        server.listen(0, "127.0.0.1", () => resolve({ server, connections: () => connections }));
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
        new Promise<{ server: NodeWS.WebSocketServer; requests: string[]; received: string[] }>(
          (resolve) => {
            const requests: string[] = [];
            const received: string[] = [];
            const server: NodeWS.WebSocketServer = new NodeWS.WebSocketServer(
              { port: 0, host: "127.0.0.1" },
              () => resolve({ server, requests, received }),
            );
            let replied = 0;
            server.on("connection", (socket: NodeWS.WebSocket, request: { url?: string }) => {
              requests.push(request.url ?? "");
              socket.on("message", (data: Buffer) => {
                received.push(data.toString("latin1"));
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
          },
        ),
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

/** Routes a stand-in session through the tunnel; returns its proxy port and Chromium's credentials. */
const routeThroughTunnel = (tunnelUrl: string | null) =>
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
            Layer.mock(ElectronWindow.ElectronWindow, {
              currentMainOrFirst: Effect.succeedSome(window as unknown as Electron.BrowserWindow),
            }),
          ),
        ),
      ),
    );
    const tunnel = Context.get(services, LoopbackTunnel.LoopbackTunnel);
    yield* Queue.take(accessRequests).pipe(
      Effect.flatMap((requestId) => tunnel.resolveAccess({ requestId, url: tunnelUrl })),
      Effect.forever,
      Effect.forkScoped,
    );
    let proxyRules = "";
    const session = {
      setProxy: async (config: Electron.ProxyConfig) => {
        proxyRules = config.proxyRules ?? "";
      },
      webRequest: { onBeforeRequest: () => undefined },
    };
    yield* tunnel.route({
      environmentId: "remote",
      session: session as unknown as Electron.Session,
    });
    const proxyPort = Number(/^http:\/\/127\.0\.0\.1:(\d+)$/.exec(proxyRules)![1]);
    const authorization = challenge(login!, {
      isProxy: true,
      scheme: "basic",
      host: "127.0.0.1",
      port: proxyPort,
    });
    return { proxyPort, authorization: authorization!, login: login! };
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
])(
  "carries a CONNECT to $authority over the tunnel WebSocket, not to local listeners",
  ({ authority, host }) =>
    Effect.gen(function* () {
      const decoy = yield* listenDecoy;
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
    const decoy = yield* listenDecoy;
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
