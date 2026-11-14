#!/usr/bin/env node

// Renders the mochi from the Icon Composer project without the glass, rim, or mask the OS adds to
// app icons: the bare mochi for Android's adaptive icon and the splash screens, and a flat red
// tile for the places the app shows its own icon.

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Console from "effect/Console";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import sharp from "sharp";

import { BRAND_ASSET_PATHS, DEVELOPMENT_PUBLIC_ICON_OVERRIDES } from "./lib/brand-assets.ts";

const ICON_PROJECT = BRAND_ASSET_PATHS.productionIconComposerProject;
const SVG_DENSITY = 300;

// Mochi width as a fraction of the canvas. Android keeps the central 72 of 108dp for launcher
// icons and the central 192 of 288dp for the splash icon.
const BARE_MOCHI = [
  { file: "apps/mobile/assets/android-icon-foreground.png", canvas: 432, fraction: 0.454 },
  { file: "apps/mobile/assets/splash-icon.png", canvas: 1152, fraction: 0.39 },
] as const;
const FLAT_TILE_SIZE = 180;
const FLAT_TILES = [
  BRAND_ASSET_PATHS.developmentSplashLogoPng,
  BRAND_ASSET_PATHS.productionSplashLogoPng,
  ...DEVELOPMENT_PUBLIC_ICON_OVERRIDES.map((override) => override.targetRelativePath),
];

const IconFill = Schema.Struct({ fill: Schema.Struct({ solid: Schema.String }) });
const decodeIconFill = Schema.decodeUnknownEffect(Schema.fromJsonString(IconFill));

export class MochiAssetRenderError extends Schema.TaggedError<MochiAssetRenderError>()(
  "MochiAssetRenderError",
  { asset: Schema.String, cause: Schema.Defect() },
) {}

// Icon Composer stores the fill as `srgb:r,g,b,a` with 0-1 channels.
const fillToHex = (solid: string) =>
  `#${solid
    .replace(/^srgb:/, "")
    .split(",")
    .slice(0, 3)
    .map((channel) =>
      Math.round(Number(channel) * 255)
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")}`;

const renderBareMochi = (asset: string, svg: Buffer, canvas: number, fraction: number) =>
  Effect.tryPromise({
    try: async () => {
      const trimmed = await sharp(svg, { density: SVG_DENSITY }).trim().png().toBuffer();
      const mochi = await sharp(trimmed)
        .resize({ width: Math.round(canvas * fraction) })
        .png()
        .toBuffer({ resolveWithObject: true });
      return sharp({
        create: { width: canvas, height: canvas, channels: 4, background: "#00000000" },
      })
        .composite([
          {
            input: mochi.data,
            left: Math.round((canvas - mochi.info.width) / 2),
            top: Math.round((canvas - mochi.info.height) / 2),
          },
        ])
        .png()
        .toBuffer();
    },
    catch: (cause) => new MochiAssetRenderError({ asset, cause }),
  });

// The mochi keeps its place in the icon canvas, so the tile matches the app icon's composition.
const renderFlatTile = (svg: Buffer, size: number, background: string) =>
  Effect.tryPromise({
    try: async () => {
      const mochi = await sharp(svg, { density: SVG_DENSITY }).resize(size, size).png().toBuffer();
      return sharp({ create: { width: size, height: size, channels: 4, background } })
        .composite([{ input: mochi }])
        .png()
        .toBuffer();
    },
    catch: (cause) => new MochiAssetRenderError({ asset: "flat tile", cause }),
  });

const exportMochiAssets = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const repositoryRoot = path.resolve(import.meta.dirname, "..");
  const project = path.join(repositoryRoot, ICON_PROJECT);
  const svg = Buffer.from(yield* fs.readFileString(path.join(project, "Assets", "mochi.svg")));
  const { fill } = yield* decodeIconFill(yield* fs.readFileString(path.join(project, "icon.json")));

  const outputs: Array<readonly [string, Buffer]> = [];
  for (const layer of BARE_MOCHI) {
    outputs.push([
      layer.file,
      yield* renderBareMochi(layer.file, svg, layer.canvas, layer.fraction),
    ]);
  }
  const tile = yield* renderFlatTile(svg, FLAT_TILE_SIZE, fillToHex(fill.solid));
  for (const file of FLAT_TILES) outputs.push([file, tile]);

  for (const [file, contents] of outputs) {
    yield* fs.writeFile(path.join(repositoryRoot, file), contents);
    yield* Console.log(`wrote ${file}`);
  }
});

if (import.meta.main) {
  exportMochiAssets.pipe(Effect.provide(NodeServices.layer), NodeRuntime.runMain);
}
