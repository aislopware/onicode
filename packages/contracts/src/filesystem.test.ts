import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import { FilesystemBrowseError, FilesystemBrowseResult } from "./filesystem.ts";

describe("FilesystemBrowseError", () => {
  it("derives a stable message from browse context while retaining the cause", () => {
    const cause = new Error("sensitive filesystem detail");
    const error = new FilesystemBrowseError({
      cwd: "/workspace",
      partialPath: "./src/mai",
      failure: "read_directory_failed",
      parentPath: "/workspace/src",
      cause,
    });

    expect(error.message).toBe("Failed to browse filesystem path './src/mai' from '/workspace'.");
    expect(error.message).not.toContain(cause.message);
    expect(error.cause).toBe(cause);
  });

  it("decodes legacy message-only errors during rolling upgrades", () => {
    const decodeError = Schema.decodeUnknownSync(FilesystemBrowseError);
    const error = decodeError({
      _tag: "FilesystemBrowseError",
      message: "Legacy filesystem browse failure.",
    });

    expect(error.message).toBe("Legacy filesystem browse failure.");
    expect(error.partialPath).toBeUndefined();
    expect(error.failure).toBeUndefined();
  });
});

describe("FilesystemBrowseResult", () => {
  it("keeps a space that ends a listed folder's path", () => {
    const schema = FilesystemBrowseResult;
    const result = Schema.decodeSync(schema)(
      Schema.encodeSync(schema)({
        parentPath: "/workspace/foo ",
        entries: [{ name: "bar ", fullPath: "/workspace/foo /bar " }],
      }),
    );

    expect(result.parentPath).toBe("/workspace/foo ");
    // Clients browse into a row by its name.
    expect(result.entries[0]?.name).toBe("bar ");
    expect(result.entries[0]?.fullPath).toBe("/workspace/foo /bar ");
  });
});
