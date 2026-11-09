#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import { Argument, Command } from "effect/cli";
import { resolveWebIconOverrides, type WebAssetBrand } from "./lib/brand-assets.ts";

const WEB_ASSET_BRANDS = [
  "development",
  "production",
] as const satisfies ReadonlyArray<WebAssetBrand>;

export const applyWebBrandAssets = Effect.fn("applyWebBrandAssets")(function* (
  brand: WebAssetBrand,
  targetDirectory: string,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));

  yield* Effect.forEach(
    resolveWebIconOverrides(brand, path.resolve(repoRoot, targetDirectory)),
    (override) =>
      fs.copyFile(path.join(repoRoot, override.sourceRelativePath), override.targetRelativePath),
    { concurrency: "unbounded" },
  );
});

export const applyWebBrandAssetsCommand = Command.make(
  "apply-web-brand-assets",
  {
    brand: Argument.Literals("brand", WEB_ASSET_BRANDS).pipe(
      Argument.withDescription("Asset brand to copy into the built renderer."),
      Argument.optional,
    ),
    targetDirectory: Argument.String("target-directory").pipe(
      Argument.withDescription("Output directory of the built renderer."),
      Argument.optional,
    ),
  },
  ({ brand, targetDirectory }) =>
    applyWebBrandAssets(
      Option.getOrElse(brand, () => "production" as const),
      Option.getOrElse(targetDirectory, () => "apps/web/dist"),
    ),
).pipe(Command.withDescription("Copy the branded splash logo into a built renderer."));

if (import.meta.main) {
  Command.run(applyWebBrandAssetsCommand, { version: "0.0.0" }).pipe(
    Effect.provide(NodeServices.layer),
    NodeRuntime.runMain,
  );
}
