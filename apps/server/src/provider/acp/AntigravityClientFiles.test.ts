import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import { readAntigravityClientTextFile } from "./AntigravityClientFiles.ts";

it.layer(NodeServices.layer)("readAntigravityClientTextFile", (it) => {
  it.effect("refuses reads when the session cwd is empty, not the server's own cwd", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      // Every package has one, so the server's process.cwd() holds a readable file.
      const serverFile = path.join(process.cwd(), "package.json");
      assert.isTrue(yield* fileSystem.exists(serverFile));
      for (const requestPath of ["package.json", serverFile]) {
        const read = yield* readAntigravityClientTextFile({
          fileSystem,
          path,
          cwd: "",
          allowedRoots: [""],
          request: { sessionId: "session-1", path: requestPath },
        }).pipe(Effect.exit);
        assert.isTrue(Exit.isFailure(read), `${requestPath} is refused`);
      }
    }),
  );
});
