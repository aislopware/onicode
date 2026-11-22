import { expect, it } from "vite-plus/test";

import { readFileAsDataUrl } from "~/lib/imageCompression";

import { readMediaPng } from "./mediaContent";

async function readDisplayP3Pixel(image: Blob): Promise<number[]> {
  const bitmap = await createImageBitmap(image);
  const context = new OffscreenCanvas(1, 1).getContext("2d", { colorSpace: "display-p3" });
  if (!context) throw new Error("no 2d context");
  context.drawImage(bitmap, 0, 0, 1, 1);
  bitmap.close();
  return Array.from(
    context.getImageData(0, 0, 1, 1, { colorSpace: "display-p3" }).data.slice(0, 3),
  );
}

// P3 green lies outside sRGB, so an sRGB canvas clips it.
it.each([
  ["Display P3", "display-p3", "color(display-p3 0 1 0)"],
  ["sRGB", "srgb", "#ff0000"],
] as const)("copies a %s JPEG as a PNG in its own colors", async (_, colorSpace, fill) => {
  const canvas = new OffscreenCanvas(8, 8);
  const context = canvas.getContext("2d", { colorSpace });
  if (!context) throw new Error("no 2d context");
  context.fillStyle = fill;
  context.fillRect(0, 0, 8, 8);
  const jpeg = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.95 });

  const png = await readMediaPng(await readFileAsDataUrl(jpeg));

  expect(png.type).toBe("image/png");
  const expected = await readDisplayP3Pixel(jpeg);
  (await readDisplayP3Pixel(png)).forEach((channel, index) =>
    expect(Math.abs(channel - expected[index]!)).toBeLessThanOrEqual(2),
  );
});
