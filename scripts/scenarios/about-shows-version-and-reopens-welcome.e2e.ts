import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { APP_DIR } from "../e2e.config.ts";
import { runPromise, test, WEB } from "./onicode.ts";

const desktopVersion = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const manifest = (yield* Path.Path).join(APP_DIR, "apps/desktop/package.json");
  const decode = Schema.decodeUnknownEffect(
    Schema.fromJsonString(Schema.Struct({ version: Schema.String })),
  );
  return (yield* decode(yield* fs.readFileString(manifest))).version;
});

test(
  "About shows the running version, has no update feed in dev, and reopens the welcome page",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/about");
    const version = await runPromise(desktopVersion);
    await expect(screen.getByRole("button", `Copy version ${version}`)).toBeVisible();
    await expect(screen.getByRole("button", "Check for Updates")).toBeDisabled();
    await app.screenshot("about");

    await screen.getByRole("link", "Open").tap();
    await expect(screen.getByRole("heading", "Welcome to Onicode")).toBeVisible();
    await screen.getByRole("button", "Start").tap();
    await expect(screen.getByRole("navigation", "Threads")).toBeVisible();
    await expect(screen.getByRole("heading", "Welcome to Onicode")).toBeHidden();
  },
);
