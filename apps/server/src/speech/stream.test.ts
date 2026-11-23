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

const audioFrame = new Uint8Array(new Float32Array([0.25]).buffer);
const finishFrame = JSON.stringify({ type: "finish" });
const answered = Effect.succeed({ revision: 1, text: null });

interface SocketCase {
  readonly name: string;
  readonly frames: ReadonlyArray<string | Uint8Array>;
  /** Runs after the stream's resources are taken, before it is handed out. */
  readonly opening?: Effect.Effect<void>;
  readonly feed: ReturnType<SpeechStream["feed"]>;
  readonly unwritable?: boolean;
  readonly silence: Parameters<typeof TestClock.adjust>[0];
  readonly sent: ReadonlyArray<string>;
  readonly released: boolean;
}

it.effect.each<SocketCase>([
  {
    name: "keeps a started client that is still waiting on the microphone",
    frames: [],
    feed: answered,
    silence: "1 minute",
    sent: ["ready"],
    released: false,
  },
  {
    name: "releases a started client that never sends audio",
    frames: [],
    feed: answered,
    silence: "3 minutes",
    sent: ["ready"],
    released: true,
  },
  {
    name: "releases the stream when a client goes silent after audio",
    frames: [audioFrame],
    feed: answered,
    silence: "1 minute",
    sent: ["ready", "update"],
    released: true,
  },
  {
    name: "waits on a slow engine that answers within its bound",
    frames: [audioFrame],
    feed: Effect.sleep("90 seconds").pipe(Effect.andThen(answered)),
    silence: "100 seconds",
    sent: ["ready", "update"],
    released: false,
  },
  {
    name: "ends the stream when a feed never answers",
    frames: [audioFrame],
    feed: Effect.never,
    silence: "3 minutes",
    sent: ["ready", "error"],
    released: true,
  },
  {
    name: "ends the stream when finishing never answers",
    frames: [audioFrame, finishFrame],
    feed: answered,
    silence: "3 minutes",
    sent: ["ready", "update", "error"],
    released: true,
  },
  {
    name: "ends the stream when a feed dies",
    frames: [audioFrame],
    feed: Effect.die("engine crashed"),
    silence: "1 minute",
    sent: ["ready", "error"],
    released: true,
  },
  {
    name: "ends the stream when starting never answers",
    frames: [],
    opening: Effect.never,
    feed: answered,
    silence: "5 minutes",
    sent: ["error"],
    released: true,
  },
  {
    name: "releases an ended stream whose client keeps the socket open",
    frames: [audioFrame],
    feed: Effect.die("engine crashed"),
    silence: "10 seconds",
    sent: ["ready", "error"],
    released: true,
  },
  {
    name: "releases a stream whose socket cannot be written",
    frames: [],
    feed: answered,
    unwritable: true,
    silence: "1 minute",
    sent: [],
    released: true,
  },
])("$name", ({ frames, opening = Effect.void, feed, unwritable, silence, sent, released }) =>
  Effect.gen(function* () {
    const releasedStream = yield* Deferred.make<void>();
    const events: string[] = [];
    let pulls = 0;
    const start = JSON.stringify({ type: "start", options: DEFAULT_SPEECH_TRANSCRIPTION_OPTIONS });
    const socket = Socket.make({
      reader: Effect.succeed({
        pull: Effect.suspend(() =>
          pulls++ === 0 ? Effect.succeed([start, ...frames] as const) : Effect.never,
        ),
        upgrade: () => Effect.void,
      }),
      writer: Effect.succeed({
        write: (chunk: Uint8Array | string | Socket.CloseEvent) =>
          unwritable
            ? Effect.fail(
                new Socket.SocketError({ reason: new Socket.SocketWriteError({ cause: "gone" }) }),
              )
            : Effect.sync(() => {
                if (typeof chunk === "string") events.push(decodeEvent(chunk).type);
              }),
        writeAll: () => Effect.void,
      }),
    });
    // The clock moves only once the socket reached the call that the last frame makes.
    const reached = yield* Deferred.make<void>();
    const last = frames.at(-1);
    const signal = (kind: "start" | "feed" | "finish") =>
      (last === undefined ? "start" : last === finishFrame ? "finish" : "feed") === kind
        ? Deferred.succeed(reached, undefined)
        : Effect.void;
    const stream: SpeechStream = {
      feed: () => signal("feed").pipe(Effect.andThen(feed)),
      finish: signal("finish").pipe(Effect.andThen(Effect.never)),
    };
    yield* runSpeechSocket(socket, {
      startStream: () =>
        Effect.acquireRelease(signal("start"), () =>
          Deferred.succeed(releasedStream, undefined),
        ).pipe(Effect.andThen(opening), Effect.as(stream)),
    }).pipe(Effect.scoped, Effect.forkChild);

    yield* Deferred.await(reached);
    yield* TestClock.adjust(silence);
    expect(events).toEqual(sent);
    expect(yield* Deferred.isDone(releasedStream)).toBe(released);
  }),
);
