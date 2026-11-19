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
 * answers the first frame with `reply` and closes.
 */
const listenEndpoint = (reply?: string) =>
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
            server.on("connection", (socket: NodeWS.WebSocket, request: { url?: string }) => {
              requests.push(request.url ?? "");
              socket.on("message", (data: Buffer) => {
                received.push(data.toString("latin1"));
                if (reply === undefined) socket.send(Buffer.concat([Uint8Array.of(0xee), data]));
                else socket.send(Buffer.from(reply, "latin1"), () => socket.close());
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

it.effect("forwards a plain http request in origin form, one request per connection", () =>
  Effect.gen(function* () {
    const endpoint = yield* listenEndpoint(
      "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\n\r\nok",
    );
    const { proxyPort, authorization } = yield* routeThroughTunnel(
      `ws://127.0.0.1:${addressPort(endpoint.server)}/api/preview/tunnel?wsTicket=t`,
    );
    const { head } = yield* sendToProxy(
      proxyPort,
      "GET http://localhost:5173/app?x=1 HTTP/1.1\r\nHost: localhost:5173\r\n" +
        `Proxy-Connection: keep-alive\r\nProxy-Authorization: ${authorization}\r\n` +
        "Accept: */*\r\n\r\n",
    );
    // The credentials stay on this machine, and the environment closes after one response.
    expect(endpoint.received.join("")).toBe(
      "GET /app?x=1 HTTP/1.1\r\nHost: localhost:5173\r\nAccept: */*\r\nConnection: close\r\n\r\n",
    );
    expect(endpoint.requests).toEqual(["/api/preview/tunnel?wsTicket=t&port=5173"]);
    expect(head).toBe("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close");
  }),
);

it.effect.each([
  { case: "a client without credentials", request: "connect-anonymous", reply: "407" },
  { case: "a client with other credentials", request: "connect-wrong", reply: "407" },
  { case: "a SOCKS client", request: "socks", reply: "400" },
  { case: "a non-loopback destination", request: "connect-lan", reply: "403" },
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
  }),
);
