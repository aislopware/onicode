import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import { claudeOutdatedBuild, readClaudeInstalledBuild } from "./ClaudeInstalledBuild.ts";

// The native installer's link layout is POSIX; Windows resolves `claude` through PATHEXT.
it.effect.skipIf(HostProcess.Platform.defaultValue() === "win32")(
  "an update that repoints the native installer's link to a new version file",
  () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "oni-claude-link-" });
      const bin = path.join(root, "bin");
      const link = path.join(bin, "claude");
      const install = Effect.fnUntraced(function* (version: string) {
        const file = path.join(root, "versions", version);
        yield* fileSystem.makeDirectory(path.dirname(file), { recursive: true });
        yield* fileSystem.writeFileString(file, "#!/bin/sh\n");
        yield* fileSystem.chmod(file, 0o755);
        yield* fileSystem.remove(link, { force: true });
        yield* fileSystem.symlink(file, link);
      });
      yield* fileSystem.makeDirectory(bin);
      yield* install("1.0.0");
      const read = readClaudeInstalledBuild("claude", { PATH: bin });
      const atInit = yield* read;
      assert.isDefined(atInit.executable);
      const outdated = Effect.map(read, (current) =>
        claudeOutdatedBuild({ atInit, current, runningVersion: "1.0.0" }),
      );

      assert.isUndefined(yield* outdated);
      yield* install("1.0.1");
      assert.deepEqual(yield* outdated, { reason: "executable_replaced" });
    }).pipe(Effect.scoped, Effect.provide(NodeServices.layer)),
);
