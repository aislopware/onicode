import {
  AuthOrchestrationOperateScope,
  SPEECH_STREAM_PATH,
  SPEECH_STREAM_MAX_CHUNK_BYTES,
  SpeechStreamCommand,
  SPEECH_MAX_OPTIONS_BYTES,
  type SpeechStreamEvent,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import { HttpRouter, HttpServerRequest, HttpServerResponse } from "effect/http";
import * as Socket from "effect/socket/Socket";
import { authenticateMediaRequest, failEnvironmentInternal } from "../auth/http.ts";
import * as SessionStore from "../auth/SessionStore.ts";
import * as SpeechService from "./SpeechService.ts";

// A recording client streams audio continuously; behind a proxy a vanished
// client never closes the server side, so silence is the only sign it is gone.
const SPEECH_SOCKET_IDLE_TIMEOUT = "15 seconds";
// Desktop opens the stream while a first-use microphone prompt may still be on screen.
const SPEECH_SOCKET_FIRST_AUDIO_TIMEOUT = "2 minutes";
// The client gives up on an unanswered start or frame after two minutes, so a later answer is
// never read.
const SPEECH_ANSWER_TIMEOUT = "2 minutes";

const decodeCommand = Schema.decodeUnknownEffect(Schema.fromJsonString(SpeechStreamCommand));

/** One socket owns one stream. Audio is processed and acknowledged in arrival order. */
export const runSpeechSocket = Effect.fn("speech.runSocket")(function* (
  socket: Socket.Socket,
  speech: Pick<SpeechService.SpeechService["Service"], "startStream">,
) {
  const scope = yield* Scope.Scope;
  const writer = yield* socket.writer;
  const send = (event: SpeechStreamEvent) => writer.write(JSON.stringify(event));
  let stream: SpeechService.SpeechStream | undefined;
  let ended = false;
  const fail = Effect.fn("speech.failSocket")(function* (message: string) {
    if (ended) return;
    ended = true;
    yield* send({ type: "error", message });
    yield* writer.write(new Socket.CloseEvent(1008, "Speech stream ended"));
  });
  const receive = (data: string | Uint8Array) => {
    if (ended) return Effect.void;
    return Effect.gen(function* () {
      if (typeof data === "string") {
        if (new TextEncoder().encode(data).byteLength > SPEECH_MAX_OPTIONS_BYTES)
          return yield* fail("Invalid speech stream command.");
        const command = yield* decodeCommand(data);
        if (command.type === "start") {
          if (stream) return yield* fail("Speech stream already started.");
          stream = yield* speech
            .startStream(command.options)
            .pipe(Scope.provide(scope), Effect.timeout(SPEECH_ANSWER_TIMEOUT));
          yield* send({ type: "ready" });
          return;
        }
        if (!stream) return yield* fail("Start speech transcription before finishing.");
        const text = yield* stream.finish.pipe(Effect.timeout(SPEECH_ANSWER_TIMEOUT));
        ended = true;
        yield* send({ type: "finished", text });
        yield* writer.write(new Socket.CloseEvent(1000));
      } else {
        if (data.byteLength > SPEECH_STREAM_MAX_CHUNK_BYTES)
          return yield* fail("Speech audio chunk is too large.");
        if (!stream) return yield* fail("Start speech transcription before sending audio.");
        const update = yield* stream.feed(data).pipe(Effect.timeout(SPEECH_ANSWER_TIMEOUT));
        yield* send({ type: "update", ...update });
      }
    }).pipe(
      // A defect or an unwritable socket must not end the loop that answers frames.
      Effect.catchCause((cause) =>
        Effect.logError("live transcription operation failed", { cause }).pipe(
          Effect.andThen(fail("Live transcription failed. Please try recording again.")),
          Effect.ignore,
        ),
      ),
    );
  };
  const reader = yield* socket.reader;
  // Silence only counts while every received frame has been answered.
  let unanswered = 0;
  let heardAudio = false;
  const activity = yield* Queue.sliding<void>(1);
  const idle = Effect.gen(function* () {
    while (true) {
      const heard = yield* Queue.take(activity).pipe(
        Effect.timeoutOption(
          heardAudio ? SPEECH_SOCKET_IDLE_TIMEOUT : SPEECH_SOCKET_FIRST_AUDIO_TIMEOUT,
        ),
      );
      // An ended stream answers nothing more, though its socket may stay open.
      if (unanswered === 0 && (ended || Option.isNone(heard))) return;
    }
  });
  const read = Effect.gen(function* () {
    const pending = yield* Queue.bounded<string | Uint8Array>(2);
    yield* Effect.forever(
      Queue.take(pending).pipe(
        Effect.flatMap(receive),
        Effect.andThen(() => {
          unanswered--;
          return Queue.offer(activity, undefined);
        }),
      ),
    ).pipe(Effect.forkScoped);
    while (true) {
      const frames = yield* reader.pull;
      unanswered += frames.length;
      heardAudio ||= frames.some((frame) => typeof frame !== "string");
      yield* Queue.offer(activity, undefined);
      yield* Effect.forEach(frames, (frame) => Queue.offer(pending, frame), {
        discard: true,
      });
    }
  });
  yield* Effect.raceFirst(read, idle).pipe(
    Effect.timeoutOrElse({
      duration: "10 minutes",
      // The socket read loop is already interrupted here; its writer can no longer send.
      orElse: () => Effect.void,
    }),
    Effect.ignore,
  );
});

export const speechStreamRouteLayer = Layer.unwrap(
  Effect.gen(function* () {
    const speech = yield* SpeechService.SpeechService;
    const sessions = yield* SessionStore.SessionStore;
    return HttpRouter.add(
      "GET",
      SPEECH_STREAM_PATH,
      Effect.gen(function* () {
        const request = yield* HttpServerRequest.HttpServerRequest;
        const session = yield* authenticateMediaRequest(AuthOrchestrationOperateScope);
        const socket = yield* request.upgrade;
        yield* Effect.raceFirst(
          runSpeechSocket(socket, speech).pipe(Effect.scoped),
          sessions.awaitInvalidation(session.sessionId).pipe(
            Effect.catchTags({
              SessionCredentialVerificationError: (error) =>
                failEnvironmentInternal("internal_error", error).pipe(Effect.ignore),
            }),
          ),
        );
        return HttpServerResponse.empty();
      }),
    );
  }),
);
