import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import { it } from "@effect/vitest";
import { expect } from "vite-plus/test";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as Socket from "effect/socket/Socket";
import * as TestClock from "effect/testing/TestClock";
import { HttpClient, HttpRouter, HttpServer } from "effect/http";
import {
  AuthOrchestrationOperateScope,
  AuthSessionId,
  DEFAULT_SPEECH_TRANSCRIPTION_OPTIONS,
  SPEECH_STREAM_PATH,
  SpeechStreamEvent,
} from "@onicode/contracts";
import * as EnvironmentAuth from "../auth/EnvironmentAuth.ts";
import * as SessionStore from "../auth/SessionStore.ts";
import { SpeechService, type SpeechStream } from "./SpeechService.ts";
import { runSpeechSocket, speechStreamRouteLayer } from "./stream.ts";
import type * as NetAddress from "effect/net/NetAddress";

const decodeEvent = Schema.decodeUnknownSync(Schema.fromJsonString(SpeechStreamEvent));

function client(url: string) {
  const socket = new WebSocket(url);
  socket.addEventListener("open", () =>
    socket.send(JSON.stringify({ type: "start", options: DEFAULT_SPEECH_TRANSCRIPTION_OPTIONS })),
  );
  const events: SpeechStreamEvent[] = [];
  let waiter: ReturnType<typeof Promise.withResolvers<SpeechStreamEvent>> | undefined;
  socket.addEventListener("message", ({ data }) => {
    const event = decodeEvent(data);
    if (waiter) {
      waiter.resolve(event);
      waiter = undefined;
    } else events.push(event);
  });
  socket.addEventListener("error", () => waiter?.reject(new Error("Speech socket failed.")));
  socket.addEventListener("close", () => waiter?.reject(new Error("Speech socket closed.")));
  return {
    socket,
    next: () => {
      const event = events.shift();
      if (event) return Promise.resolve(event);
      waiter = Promise.withResolvers<SpeechStreamEvent>();
      return waiter.promise;
    },
  };
}

const withServer = Effect.fn("test.speech.withServer")(function* (
  startStream: SpeechService["Service"]["startStream"],
  run: (url: string) => Promise<void>,
  sessionInvalidated: Effect.Effect<void> = Effect.never,
  authenticated: EnvironmentAuth.EnvironmentAuth["Service"]["authenticateWebSocketUpgrade"] = () =>
    Effect.succeed({
      sessionId: AuthSessionId.make("speech-test"),
      subject: "speech-test",
      method: "bearer-access-token",
      scopes: [AuthOrchestrationOperateScope],
    }),
) {
  yield* Layer.build(
    HttpRouter.serve(speechStreamRouteLayer, { disableLogger: true, disableListenLog: true }).pipe(
      Layer.provide(Layer.mock(SpeechService, { startStream })),
      Layer.provide(
        Layer.mock(SessionStore.SessionStore, { awaitInvalidation: () => sessionInvalidated }),
      ),
      Layer.provide(
        Layer.mock(EnvironmentAuth.EnvironmentAuth, {
          authenticateWebSocketUpgrade: authenticated,
        }),
      ),
    ),
  );
  const server = yield* HttpServer.HttpServer;
  const address = server.address as NetAddress.InetAddress;
  const host = address._tag === "InetAddressV6" ? "[::1]" : "127.0.0.1";
  const url = `ws://${host}:${address.port}${SPEECH_STREAM_PATH}`;
  yield* Effect.promise(() => run(url));
});

it.live("acknowledges binary audio and returns the finalized transcript over a real socket", () => {
  const closed = Promise.withResolvers<void>();
  const received: number[] = [];
  const stream: SpeechStream = {
    feed: (bytes) =>
      Effect.sync(() => {
        received.push(
          new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getFloat32(0, true),
        );
        return { revision: received.length, text: { committed: "hello ", tentative: "world" } };
      }),
    finish: Effect.sync(() => {
      expect(received).toEqual([0.25, 0.5]);
      return "hello world";
    }),
  };
  return withServer(
    () => Effect.acquireRelease(Effect.succeed(stream), () => Effect.sync(() => closed.resolve())),
    async (url) => {
      const connection = client(url);
      try {
        expect(await connection.next()).toEqual({ type: "ready" });
        for (const value of [0.25, 0.5]) {
          connection.socket.send(new Float32Array([value]));
          expect(await connection.next()).toMatchObject({
            type: "update",
            text: { tentative: "world" },
          });
        }
        connection.socket.send(JSON.stringify({ type: "finish" }));
        expect(await connection.next()).toEqual({ type: "finished", text: "hello world" });
        await closed.promise;
      } finally {
        connection.socket.close();
      }
    },
  ).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest));
});

it.live("releases in-flight inference after disconnect", () => {
  const feeding = Promise.withResolvers<void>();
  const closed = Promise.withResolvers<void>();
  const stream: SpeechStream = {
    feed: () => Effect.sync(() => feeding.resolve()).pipe(Effect.andThen(Effect.never)),
    finish: Effect.succeed("unexpected"),
  };
  return withServer(
    () => Effect.acquireRelease(Effect.succeed(stream), () => Effect.sync(() => closed.resolve())),
    async (url) => {
      const connection = client(url);
      try {
        await connection.next();
        connection.socket.send(new Float32Array([0.25]));
        await feeding.promise;
        connection.socket.close();
        await closed.promise;
      } finally {
        connection.socket.close();
      }
    },
  ).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest));
});

it.live("processes pipelined audio before the finish command", () => {
  const firstFeed = Promise.withResolvers<void>();
  const releaseFirst = Promise.withResolvers<void>();
  const received: number[] = [];
  const stream: SpeechStream = {
    feed: (bytes) =>
      Effect.promise(async () => {
        const value = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getFloat32(
          0,
          true,
        );
        received.push(value);
        if (received.length === 1) {
          firstFeed.resolve();
          await releaseFirst.promise;
        }
        return { revision: received.length, text: null };
      }),
    finish: Effect.sync(() => {
      expect(received).toEqual([0.25, 0.5]);
      return "done";
    }),
  };
  return withServer(
    () => Effect.succeed(stream),
    async (url) => {
      const connection = client(url);
      try {
        expect(await connection.next()).toEqual({ type: "ready" });
        connection.socket.send(new Float32Array([0.25]));
        await firstFeed.promise;
        connection.socket.send(new Float32Array([0.5]));
        connection.socket.send(JSON.stringify({ type: "finish" }));
        releaseFirst.resolve();
        expect(await connection.next()).toMatchObject({ type: "update", revision: 1 });
        expect(await connection.next()).toMatchObject({ type: "update", revision: 2 });
        expect(await connection.next()).toEqual({ type: "finished", text: "done" });
      } finally {
        connection.socket.close();
      }
    },
  ).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest));
});

it.live("closes the socket and releases its stream when the session is revoked", () => {
  const revoked = Promise.withResolvers<void>();
  const closed = Promise.withResolvers<void>();
  const stream: SpeechStream = { feed: () => Effect.never, finish: Effect.never };
  return withServer(
    () => Effect.acquireRelease(Effect.succeed(stream), () => Effect.sync(() => closed.resolve())),
    async (url) => {
      const connection = client(url);
      try {
        expect(await connection.next()).toEqual({ type: "ready" });
        const socketClosed = new Promise((resolve) =>
          connection.socket.addEventListener("close", resolve),
        );
        revoked.resolve();
        await Promise.all([socketClosed, closed.promise]);
      } finally {
        connection.socket.close();
      }
    },
    Effect.promise(() => revoked.promise),
  ).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest));
});

it.live("answers a failed session lookup as a server error, not a rejected credential", () =>
  Effect.gen(function* () {
    yield* withServer(
      () => Effect.never,
      async () => {},
      Effect.never,
      () =>
        Effect.fail(
          new EnvironmentAuth.ServerAuthSessionCredentialValidationError({ cause: "db down" }),
        ),
    );
    const response = yield* HttpClient.get(SPEECH_STREAM_PATH);
    expect(response.status).toBe(500);
  }).pipe(Effect.scoped, Effect.provide(NodeHttpServer.layerTest)),
);

it.effect("releases the stream when a started client goes silent", () =>
  Effect.gen(function* () {
    const released = yield* Deferred.make<void>();
    let pulls = 0;
    const socket = Socket.make({
      reader: Effect.succeed({
        pull: Effect.suspend(() =>
          pulls++ === 0
            ? Effect.succeed([
                JSON.stringify({ type: "start", options: DEFAULT_SPEECH_TRANSCRIPTION_OPTIONS }),
              ] as const)
            : Effect.never,
        ),
        upgrade: () => Effect.void,
      }),
      writer: Effect.succeed({ write: () => Effect.void, writeAll: () => Effect.void }),
    });
    const stream: SpeechStream = { feed: () => Effect.never, finish: Effect.never };
    yield* runSpeechSocket(socket, {
      startStream: () =>
        Effect.acquireRelease(Effect.succeed(stream), () => Deferred.succeed(released, undefined)),
    }).pipe(Effect.scoped, Effect.forkChild);

    yield* TestClock.adjust("1 minute");
    expect(yield* Deferred.isDone(released)).toBe(true);
  }),
);
