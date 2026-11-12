import { expect, it } from "@effect/vitest";
import type { RemoteDesktopHost, RemoteDesktopHostRequest } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import { TestClock } from "effect/testing";

import * as RemoteDesktop from "./RemoteDesktop.ts";

const HOST: RemoteDesktopHost = {
  displays: [{ id: "1", label: "Built-in", width: 3024, height: 1964, primary: true }],
  control: true,
};

/** Runs a host that hands each request to the test. */
const startHost = Effect.fn("test.startHost")(function* (host: RemoteDesktopHost = HOST) {
  const remoteDesktop = yield* RemoteDesktop.RemoteDesktop;
  const requests = yield* Queue.unbounded<RemoteDesktopHostRequest>();
  const registered = yield* remoteDesktop.state.pipe(
    Stream.filter((state) => state.host !== null),
    Stream.take(1),
    Stream.runDrain,
    Effect.forkChild,
  );
  const fiber = yield* remoteDesktop.host(host).pipe(
    Stream.runForEach((request) => Queue.offer(requests, request)),
    Effect.forkChild,
  );
  yield* Fiber.join(registered);
  return { requests, fiber };
});

const connect = (offer = "offer") =>
  Effect.flatMap(RemoteDesktop.RemoteDesktop, (remoteDesktop) =>
    remoteDesktop.connect({ displayId: "1", offer }, "Phone"),
  );

it.layer(RemoteDesktop.layer)("RemoteDesktop", (it) => {
  it.effect("relays a viewer's offer to the host and the host's answer back", () =>
    Effect.gen(function* () {
      const remoteDesktop = yield* RemoteDesktop.RemoteDesktop;
      const host = yield* startHost();
      const connecting = yield* Effect.forkChild(connect("v=0 offer"));
      const request = yield* Queue.take(host.requests);
      expect(request).toMatchObject({ displayId: "1", offer: "v=0 offer", viewer: "Phone" });
      yield* remoteDesktop.answer({
        requestId: request.requestId,
        result: { answer: "v=0 answer" },
      });
      expect(yield* Fiber.join(connecting)).toEqual({ answer: "v=0 answer" });
      yield* Fiber.interrupt(host.fiber);
    }),
  );

  it.effect("refuses a viewer when no host shares, when it refuses, and when it stops", () =>
    Effect.gen(function* () {
      const remoteDesktop = yield* RemoteDesktop.RemoteDesktop;
      expect((yield* Effect.flip(connect())).reason).toBe("no-host");

      const host = yield* startHost();
      const refused = yield* Effect.forkChild(Effect.flip(connect()));
      const request = yield* Queue.take(host.requests);
      yield* remoteDesktop.answer({
        requestId: request.requestId,
        result: { refused: "Screen Recording is off." },
      });
      expect(yield* Fiber.join(refused)).toMatchObject({
        reason: "refused",
        message: "Screen Recording is off.",
      });

      const abandoned = yield* Effect.forkChild(Effect.flip(connect()));
      yield* Queue.take(host.requests);
      yield* Fiber.interrupt(host.fiber);
      expect((yield* Fiber.join(abandoned)).reason).toBe("no-host");
      expect(yield* remoteDesktop.state.pipe(Stream.take(1), Stream.runCollect)).toEqual([
        { host: null },
      ]);
    }),
  );

  it.effect("gives up on a host that never answers", () =>
    Effect.gen(function* () {
      const host = yield* startHost();
      const waiting = yield* Effect.forkChild(Effect.flip(connect()));
      yield* Queue.take(host.requests);
      yield* TestClock.adjust("20 seconds");
      expect((yield* Fiber.join(waiting)).reason).toBe("timeout");
      yield* Fiber.interrupt(host.fiber);
    }),
  );

  it.effect("a newer host replaces the old one and ends its stream", () =>
    Effect.gen(function* () {
      const remoteDesktop = yield* RemoteDesktop.RemoteDesktop;
      const first = yield* startHost();
      const second = yield* startHost({ ...HOST, control: false });
      yield* Fiber.join(first.fiber);
      expect(yield* remoteDesktop.state.pipe(Stream.take(1), Stream.runCollect)).toEqual([
        { host: { ...HOST, control: false } },
      ]);
      const connecting = yield* Effect.forkChild(connect());
      const request = yield* Queue.take(second.requests);
      yield* remoteDesktop.answer({
        requestId: request.requestId,
        result: { answer: "answer" },
      });
      expect(yield* Fiber.join(connecting)).toEqual({ answer: "answer" });
      yield* Fiber.interrupt(second.fiber);
    }),
  );
});
