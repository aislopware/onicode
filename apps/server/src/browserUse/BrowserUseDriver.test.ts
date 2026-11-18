import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import * as Digest from "effect/crypto/Digest";
import * as Effect from "effect/Effect";
import * as Hex from "effect/encoding/Hex";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Stream from "effect/Stream";
import { HttpClient, HttpClientResponse } from "effect/http";

import * as BrowserUseDriver from "./BrowserUseDriver.ts";

it.layer(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))("BrowserUseDriver", (it) => {
  it.effect("installs the pinned download as the executable itself", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const digest = yield* Digest.Digest;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "oni-browser-use-driver-test-" });
      const script = "#!/bin/sh\necho obu\n";
      const binary = new TextEncoder().encode(script);
      const driver = yield* BrowserUseDriver.makeBrowserUseDriver({
        baseDir,
        release: {
          version: "1.2.3",
          url: "https://github.com/aislopware/oni-browser-use/releases/download/fixture",
          bytes: binary.byteLength,
          sha256: Hex.encode(yield* digest.digest(Digest.sha256, binary).pipe(Effect.orDie)),
        },
      }).pipe(
        Effect.provideService(
          HttpClient.HttpClient,
          HttpClient.make((request) =>
            Effect.sync(() =>
              Object.defineProperty(
                HttpClientResponse.fromWeb(request, new Response(null)),
                "stream",
                {
                  value: Stream.make(binary.subarray(0, 7), binary.subarray(7)),
                },
              ),
            ),
          ),
        ),
      );

      const executable = yield* driver.executable;

      expect(path.dirname(executable)).toBe(path.join(baseDir, "tools", "obu", "1.2.3"));
      expect(yield* fs.readFileString(executable)).toBe(script);
      expect((yield* fs.stat(executable)).mode & 0o111).not.toBe(0);
      expect(yield* driver.state).toEqual({ state: "installed" });
      expect(driver.version).toBe("1.2.3");
    }),
  );
});
