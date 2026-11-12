import {
  RemoteDesktopError,
  type RemoteDesktopAnswerInput,
  type RemoteDesktopConnectInput,
  type RemoteDesktopConnectResult,
  type RemoteDesktopHost,
  type RemoteDesktopHostRequest,
  type RemoteDesktopState,
} from "@onicode/contracts";
import type * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";

// An answer needs a screen capture to start, which the OS can hold behind a permission prompt.
const ANSWER_TIMEOUT = "20 seconds";

interface Registration {
  readonly requests: Queue.Queue<RemoteDesktopHostRequest, Cause.Done>;
  readonly pending: Map<string, Deferred.Deferred<string, RemoteDesktopError>>;
}

/**
 * Introduces a viewer to the desktop app sharing this machine's screen. Only the WebRTC
 * handshake passes through here; video and input then flow between the two peers directly.
 */
export class RemoteDesktop extends Context.Service<
  RemoteDesktop,
  {
    readonly state: Stream.Stream<RemoteDesktopState>;
    /** Registers the host for as long as the stream is consumed. A newer host replaces it. */
    readonly host: (host: RemoteDesktopHost) => Stream.Stream<RemoteDesktopHostRequest>;
    readonly answer: (input: RemoteDesktopAnswerInput) => Effect.Effect<void>;
    readonly connect: (
      input: RemoteDesktopConnectInput,
      viewer: string,
    ) => Effect.Effect<RemoteDesktopConnectResult, RemoteDesktopError>;
  }
>()("onicode/remoteDesktop/RemoteDesktop") {}

const make = Effect.gen(function* () {
  const state = yield* SubscriptionRef.make<RemoteDesktopState>({ host: null });
  let current: Registration | null = null;
  let requestCount = 0;

  const unregister = Effect.fn("RemoteDesktop.unregister")(function* (registration: Registration) {
    yield* Queue.end(registration.requests);
    yield* Effect.forEach(
      registration.pending.values(),
      (answer) => Deferred.fail(answer, new RemoteDesktopError({ reason: "no-host" })),
      { discard: true },
    );
    registration.pending.clear();
    if (current !== registration) return;
    current = null;
    yield* SubscriptionRef.set(state, { host: null });
  });

  const register = Effect.fn("RemoteDesktop.register")(function* (host: RemoteDesktopHost) {
    const registration: Registration = {
      requests: yield* Queue.unbounded<RemoteDesktopHostRequest, Cause.Done>(),
      pending: new Map(),
    };
    const previous = current;
    current = registration;
    yield* SubscriptionRef.set(state, { host });
    if (previous) yield* unregister(previous);
    return registration;
  });

  return RemoteDesktop.of({
    state: SubscriptionRef.changes(state),
    host: (host) =>
      Stream.unwrap(
        Effect.acquireRelease(register(host), unregister).pipe(
          Effect.map((registration) => Stream.fromQueue(registration.requests)),
        ),
      ).pipe(Stream.scoped),
    answer: Effect.fn("RemoteDesktop.answer")(function* (input) {
      const answer = current?.pending.get(input.requestId);
      if (!answer) return;
      yield* "answer" in input.result
        ? Deferred.succeed(answer, input.result.answer)
        : Deferred.fail(
            answer,
            new RemoteDesktopError({ reason: "refused", refusal: input.result.refused }),
          );
    }),
    connect: Effect.fn("RemoteDesktop.connect")(function* (input, viewer) {
      const registration = current;
      if (!registration) return yield* new RemoteDesktopError({ reason: "no-host" });
      const requestId = String(++requestCount);
      const answer = yield* Deferred.make<string, RemoteDesktopError>();
      registration.pending.set(requestId, answer);
      yield* Queue.offer(registration.requests, { requestId, viewer, ...input });
      return yield* Deferred.await(answer).pipe(
        Effect.timeoutOrElse({
          duration: ANSWER_TIMEOUT,
          orElse: () => Effect.fail(new RemoteDesktopError({ reason: "timeout" })),
        }),
        Effect.map((sdp) => ({ answer: sdp })),
        Effect.ensuring(Effect.sync(() => registration.pending.delete(requestId))),
      );
    }),
  });
});

export const layer = Layer.effect(RemoteDesktop, make);
