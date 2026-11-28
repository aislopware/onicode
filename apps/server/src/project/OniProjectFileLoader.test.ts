import * as NodeServices from "@effect/platform-node/NodeServices";
import { it, describe, expect } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import { vi } from "vite-plus/test";

import * as NetworkLinks from "../workspace/networkLinks.ts";
import * as OniProjectFileLoader from "./OniProjectFileLoader.ts";

vi.mock("../workspace/networkLinks.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof NetworkLinks>();
  return { ...actual, networkPathReachedBy: vi.fn(actual.networkPathReachedBy) };
});

const layerTest = Layer.empty.pipe(
  Layer.provideMerge(OniProjectFileLoader.layer),
  Layer.provideMerge(NodeServices.layer),
);

const makeTempDir = Effect.gen(function* () {
  const fileSystem = yield* FileSystem.FileSystem;
  return yield* fileSystem.makeTempDirectoryScoped({
    prefix: "onicode-project-file-",
  });
});

const writeProjectFile = Effect.fn("writeProjectFile")(function* (cwd: string, contents: string) {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  yield* fileSystem.writeFileString(path.join(cwd, "onicode.json"), contents).pipe(Effect.orDie);
});

it.layer(layerTest)("OniProjectFileLoader", (it) => {
  describe("load", () => {
    it.effect("returns none for malformed JSON without failing", () =>
      Effect.gen(function* () {
        const loader = yield* OniProjectFileLoader.OniProjectFileLoader;
        const cwd = yield* makeTempDir;
        yield* writeProjectFile(cwd, "{ not json");

        const loaded = yield* loader.load(cwd);

        expect(Option.isNone(loaded)).toBe(true);
      }),
    );

    it.effect("never reads a project file a link carries onto a share", () =>
      Effect.gen(function* () {
        const loader = yield* OniProjectFileLoader.OniProjectFileLoader;
        const path = yield* Path.Path;
        const cwd = yield* makeTempDir;
        yield* writeProjectFile(cwd, '{ "iconPath": "brand/mark.svg" }');
        expect(Option.isSome(yield* loader.load(cwd))).toBe(true);
        using _share = vi
          .mocked(NetworkLinks.networkPathReachedBy)
          .mockImplementation(async (_path, filePath) =>
            filePath === path.join(cwd, "onicode.json") ? "\\\\nas\\share\\onicode.json" : null,
          );

        expect(Option.isNone(yield* loader.load(cwd))).toBe(true);
      }),
    );
  });
});
