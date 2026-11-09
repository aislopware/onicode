import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import { parseOniProjectFile, OniProjectFileFromJson } from "./oniProjectFile.ts";

const decodeJson = Schema.decodeUnknownSync(OniProjectFileFromJson);

describe("OniProjectFileFromJson", () => {
  it("decodes lenient JSONC with comments and trailing commas", () => {
    const decoded = decodeJson(`{
      // team scripts
      "iconPath": "assets/logo.svg",
      "scripts": [
        { "name": "Dev", "command": "pnpm dev", "stopOnSettle": true, },
      ],
    }`);

    expect(decoded.iconPath).toBe("assets/logo.svg");
    expect(decoded.scripts?.[0]).toEqual({ name: "Dev", command: "pnpm dev", stopOnSettle: true });
  });
});

describe("parseOniProjectFile", () => {
  it("reads project defaults from JSONC with a URL in a block comment", () => {
    expect(
      parseOniProjectFile(`{
        /* See https://example.com/schema/onicode.json */
        "defaultThreadEnvMode": "worktree"
      }`),
    ).toEqual({ defaultThreadEnvMode: "worktree" });
  });
});
