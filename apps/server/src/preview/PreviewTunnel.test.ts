import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import * as NodeWS from "@effect/platform-node/NodeWS";
import { expect, it } from "@effect/vitest";
import {
  AuthOrchestrationReadScope,
  AuthSessionId,
  AuthTerminalOperateScope,
} from "@onicode/contracts";
// @effect-diagnostics-next-line nodeBuiltinImport:off -- A server with production's WebSocket options.
import * as NodeHttp from "node:http";
import * as NodeNet from "node:net";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { HttpRouter, HttpServer } from "effect/http";

import * as EnvironmentAuth from "../auth/EnvironmentAuth.ts";
import * as SessionStore from "../auth/SessionStore.ts";
import * as PreviewTunnel from "./PreviewTunnel.ts";

const listen = (onConnection: (socket: NodeNet.Socket) => void) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{ server: NodeNet.Server; sockets: Set<NodeNet.Socket> }>((resolve) => {
          const sockets = new Set<NodeNet.Socket>();
          const server = NodeNet.createServer((socket) => {
            sockets.add(socket);
            onConnection(socket);
          });
          server.listen(0, "127.0.0.1", () => resolve({ server, sockets }));
        }),
    ),
    ({ server, sockets }) =>
      Effect.promise(
        () =>
          new Promise<void>((resolve) => {
            for (const socket of sockets) socket.destroy();
            server.close(() => resolve());
          }),
      ),
  ).pipe(Effect.map(({ server }) => ({ port: (server.address() as NodeNet.AddressInfo).port })));

/** Serves the tunnel route; `wsTicket=reader` authenticates without terminal:operate. */
const serveTunnel = (sessionInvalidated: Effect.Effect<void> = Effect.never) =>
  Effect.gen(function* () {
    const services = yield* Layer.build(
      HttpRouter.serve(PreviewTunnel.routeLayer, { disableListenLog: true }).pipe(
        // As in production, so the route has to turn compression down itself.
        Layer.provideMerge(
          NodeHttpServer.layer(NodeHttp.createServer, {
            host: "127.0.0.1",
            port: 0,
            websocket: { perMessageDeflate: true },
          }),
        ),
        Layer.provide(PreviewTunnel.layer),
        Layer.provide(
          Layer.mock(SessionStore.SessionStore, {
            awaitInvalidation: () => sessionInvalidated,
          }),
        ),
        Layer.provide(
          Layer.mock(EnvironmentAuth.EnvironmentAuth, {
            authenticateWebSocketUpgrade: (request) =>
              Effect.succeed({
                sessionId: AuthSessionId.make("tunnel-test"),
                subject: "tunnel-test",
                method: "bearer-access-token",
                scopes: request.url.includes("wsTicket=reader")
                  ? [AuthOrchestrationReadScope]
                  : [AuthOrchestrationReadScope, AuthTerminalOperateScope],
              }),
          }),
        ),
      ),
    );
    const origin = new URL(
      HttpServer.formatAddress(Context.get(services, HttpServer.HttpServer).address),
    );
    return { origin: `ws://${origin.host}`, port: Number(origin.port) };
  });

const openTunnel = (url: string) =>
  Effect.acquireRelease(
    Effect.sync(() => new NodeWS.WebSocket(url)),
    (socket) => Effect.sync(() => socket.terminate()),
  );

/** The status the route turned a tunnel down with. */
const refusal = (client: NodeWS.WebSocket) =>
  Effect.promise(
    () =>
      new Promise<number | undefined>((resolve, reject) => {
        client.on("unexpected-response", (_request, response) => resolve(response.statusCode));
        client.on("open", () => reject(new Error("tunnel opened")));
        client.on("error", reject);
      }),
  );

it.effect("pipes bytes both ways between the WebSocket and the loopback port", () =>
  Effect.gen(function* () {
    const upstreamReceived: number[] = [];
    const upstream = yield* listen((socket) => {
      socket.write(Uint8Array.of(0x10, 0x11));
      socket.on("data", (chunk: Buffer) => {
        upstreamReceived.push(...chunk);
        socket.write(Buffer.concat([Uint8Array.of(0xee), chunk]));
      });
    });
    const tunnel = yield* serveTunnel();
    const client = yield* openTunnel(`${tunnel.origin}/api/preview/tunnel?port=${upstream.port}`);
    const clientReceived: number[] = [];
    const done = Promise.withResolvers<void>();
    client.on("error", done.reject);
    client.on("message", (data: Buffer) => {
      clientReceived.push(...data);
      if (clientReceived.length === 2) client.send(Uint8Array.of(0x01, 0x02, 0x03));
      if (clientReceived.length === 6) done.resolve();
    });
    yield* Effect.promise(() => done.promise);
    // Compression would queue frames past the TCP buffer that backpressure watches.
    expect(client.extensions).toBe("");
    expect(upstreamReceived).toEqual([0x01, 0x02, 0x03]);
    expect(clientReceived).toEqual([0x10, 0x11, 0xee, 0x01, 0x02, 0x03]);
  }),
);

it.effect.each([
  { case: "the server's own port", target: "self", ticket: "operator" },
  { case: "a session without terminal:operate", target: "listed", ticket: "reader" },
] as const)("refuses $case with 403 before dialling", ({ target, ticket }) =>
  Effect.gen(function* () {
    let dialled = 0;
    const countDial = (socket: NodeNet.Socket) => {
      dialled++;
      socket.destroy();
    };
    const listed = yield* listen(countDial);
    const tunnel = yield* serveTunnel();
    const port = { listed: listed.port, self: tunnel.port }[target];
    const client = yield* openTunnel(
      `${tunnel.origin}/api/preview/tunnel?port=${port}&wsTicket=${ticket}`,
    );
    expect(yield* refusal(client)).toBe(403);
    expect(dialled).toBe(0);
  }),
);

it.effect("answers a port nothing listens on with 502", () =>
  Effect.gen(function* () {
    const closed = yield* Effect.promise(
      () =>
        new Promise<number>((resolve) => {
          const server = NodeNet.createServer();
          server.listen(0, "127.0.0.1", () => {
            const { port } = server.address() as NodeNet.AddressInfo;
            server.close(() => resolve(port));
          });
        }),
    );
    const tunnel = yield* serveTunnel();
    const client = yield* openTunnel(`${tunnel.origin}/api/preview/tunnel?port=${closed}`);
    expect(yield* refusal(client)).toBe(502);
  }),
);

it.effect.each([
  { case: "an IPv6 loopback address", host: "::1", status: 502 },
  { case: "a non-loopback address", host: "192.168.1.20", status: 400 },
] as const)(
  "never dials 127.0.0.1 for $case, even when a server listens there",
  ({ host, status }) =>
    Effect.gen(function* () {
      let dialled = 0;
      const listed = yield* listen((socket) => {
        dialled++;
        socket.destroy();
      });
      const tunnel = yield* serveTunnel();
      const client = yield* openTunnel(
        `${tunnel.origin}/api/preview/tunnel?port=${listed.port}&host=${encodeURIComponent(host)}`,
      );
      expect(yield* refusal(client)).toBe(status);
      expect(dialled).toBe(0);
    }),
);

it.effect("closes an open tunnel when its session is revoked", () =>
  Effect.gen(function* () {
    const upstreamClosed = Promise.withResolvers<void>();
    const upstream = yield* listen((socket) => socket.on("close", () => upstreamClosed.resolve()));
    const revoked = yield* Deferred.make<void>();
    const tunnel = yield* serveTunnel(Deferred.await(revoked));
    const client = yield* openTunnel(`${tunnel.origin}/api/preview/tunnel?port=${upstream.port}`);
    yield* Effect.promise(() => new Promise((resolve) => client.once("open", resolve)));
    const clientClosed = new Promise((resolve) => client.once("close", resolve));
    yield* Deferred.succeed(revoked, undefined);
    yield* Effect.promise(() => Promise.all([clientClosed, upstreamClosed.promise]));
  }),
);
