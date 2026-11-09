import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import { beforeEach, vi } from "vite-plus/test";

const { handleMock, netFetchMock, registerSourceMock, unhandleMock } = vi.hoisted(() => ({
  handleMock: vi.fn(),
  registerSourceMock: vi.fn(),
  netFetchMock: vi.fn(),
  unhandleMock: vi.fn(),
}));

vi.mock("electron", () => ({
  net: { fetch: netFetchMock },
  protocol: { handle: handleMock, registerSource: registerSourceMock, unhandle: unhandleMock },
}));

import * as ElectronProtocol from "./ElectronProtocol.ts";

const layerProtocol = ElectronProtocol.layer;

describe("ElectronProtocol", () => {
  beforeEach(() => {
    handleMock.mockReset();
    netFetchMock.mockReset();
    registerSourceMock.mockReset();
    unhandleMock.mockReset();
  });

  it.effect("serves the bundled client from disk under the app policy, without a JS handler", () =>
    Effect.gen(function* () {
      const protocol = yield* ElectronProtocol.ElectronProtocol;
      yield* protocol.registerDesktopProtocol({ scheme: "onicode", assetDirectory: "/app/client" });

      assert.equal(handleMock.mock.calls.length, 0);
      const [scheme, source] = registerSourceMock.mock.calls[0]!;
      assert.equal(scheme, "onicode");
      const [route] = source.routes;
      assert.deepEqual(route.match, { host: "app" });
      assert.equal(route.source.root, "/app/client");
      assert.include(route.source.headers["Content-Security-Policy"], "default-src 'self'");
    }).pipe(Effect.provide(layerProtocol), Effect.scoped),
  );

  it.effect("proxies the stable renderer origin to the current app server", () =>
    Effect.gen(function* () {
      let handler: ((request: Request) => Promise<Response>) | undefined;
      handleMock.mockImplementation((_scheme, nextHandler) => {
        handler = nextHandler;
      });
      netFetchMock.mockResolvedValue(new Response("ok"));

      yield* Effect.scoped(
        Effect.gen(function* () {
          const protocol = yield* ElectronProtocol.ElectronProtocol;
          yield* protocol.registerDesktopProtocol({
            scheme: "onicode-dev",
            targetOrigin: new URL("http://127.0.0.1:3773/"),
          });
          assert.isDefined(handler);

          const response = yield* Effect.promise(() =>
            handler!(
              new Request("onicode-dev://app/api/health?verbose=1", {
                headers: {
                  accept: "application/json",
                  origin: "onicode-dev://app",
                  referer: "onicode-dev://app/",
                  "sec-fetch-site": "same-origin",
                },
              }),
            ),
          );
          assert.equal(yield* Effect.promise(() => response.text()), "ok");
          assert.include(
            response.headers.get("content-security-policy") ?? "",
            "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval';",
          );
          assert.include(
            response.headers.get("content-security-policy") ?? "",
            "connect-src 'self' blob: http: https: ws: wss:",
          );
          assert.include(
            response.headers.get("content-security-policy") ?? "",
            "img-src 'self' onicode-dev: blob: data: http: https:",
          );
          assert.include(
            response.headers.get("content-security-policy") ?? "",
            "font-src 'self' onicode-dev: data:",
          );
        }),
      );

      assert.deepEqual(
        handleMock.mock.calls.map((call) => call[0]),
        ["onicode-dev"],
      );
      assert.equal(netFetchMock.mock.calls[0]?.[0], "http://127.0.0.1:3773/api/health?verbose=1");
      const forwardedHeaders = new Headers(netFetchMock.mock.calls[0]?.[1]?.headers);
      assert.equal(forwardedHeaders.get("accept"), "application/json");
      assert.isNull(forwardedHeaders.get("origin"));
      assert.isNull(forwardedHeaders.get("referer"));
      assert.isNull(forwardedHeaders.get("sec-fetch-site"));
      assert.deepEqual(unhandleMock.mock.calls, [["onicode-dev"]]);
    }).pipe(Effect.provide(layerProtocol)),
  );

  it.effect("keeps proxied paths on the configured server", () =>
    Effect.gen(function* () {
      let handler: ((request: Request) => Promise<Response>) | undefined;
      handleMock.mockImplementation((_scheme, nextHandler) => {
        handler = nextHandler;
      });
      netFetchMock.mockImplementation(async () => new Response("ok"));

      yield* Effect.scoped(
        Effect.gen(function* () {
          const protocol = yield* ElectronProtocol.ElectronProtocol;
          yield* protocol.registerDesktopProtocol({
            scheme: "onicode-dev",
            targetOrigin: new URL("http://127.0.0.1:5733/"),
          });
          yield* Effect.promise(() =>
            handler!(new Request("onicode-dev://app//example.com/src/main.tsx?t=1")),
          );
          yield* Effect.promise(() => handler!(new Request("onicode-dev://app/\\example.com/")));
        }),
      );

      assert.deepEqual(
        netFetchMock.mock.calls.map((call) => call[0]),
        [
          "http://127.0.0.1:5733//example.com/src/main.tsx?t=1",
          "http://127.0.0.1:5733//example.com/",
        ],
      );
    }).pipe(Effect.provide(layerProtocol)),
  );

  it.effect("rejects custom protocol requests for another host", () =>
    Effect.gen(function* () {
      let handler: ((request: Request) => Promise<Response>) | undefined;
      handleMock.mockImplementation((_scheme, nextHandler) => {
        handler = nextHandler;
      });

      const response = yield* Effect.scoped(
        Effect.gen(function* () {
          const protocol = yield* ElectronProtocol.ElectronProtocol;
          yield* protocol.registerDesktopProtocol({
            scheme: "onicode",
            targetOrigin: new URL("http://127.0.0.1:3773/"),
          });
          return yield* Effect.promise(() => handler!(new Request("onicode://other/")));
        }),
      );

      assert.equal(response.status, 404);
      assert.equal(netFetchMock.mock.calls.length, 0);
    }).pipe(Effect.provide(layerProtocol)),
  );
});
