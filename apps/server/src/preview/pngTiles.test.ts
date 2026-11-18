import { expect, it } from "vite-plus/test";

import { decodePng, encodePng, joinTiles, tileClip } from "./pngTiles.ts";

// A 16x16 capture Chromium made of a gradient page: RGB, rows filtered with Paeth and Up.
const CHROMIUM_CAPTURE = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAACWUlEQVR4nDRSy27TQBQ9nozHduw83TTQh0BCVUU3" +
    "UCRWIPULEEKIBWyQWPEVLPgJVqh0y5IdO1aodBUhIiFRVYJASVNnYseP2I4fzLh0dHSlmTlz7rn3DuWDDkLAr+Bd" +
    "wq3i/PJcECJgAcSg0IEMSOVG7EAgVwnkwLI6F0gqxIgNQTHkg5OT4ugwu8oU76x8sM6UC3ZFevMnfkjYWkIiA4ku" +
    "BE0IvP2Q3L1N9+6oX37mxzyX8hfCEfZqqi3YJhYmRKThou7Ng9NpsXZNEdavr5KjUX7Is3SGv7x4TNj7Wfqko61Y" +
    "ZKEj0UDn8xZRFaiLpQ7DQkHQtxSPowNFLXAeloLkNcCa0k/CQDnvWBZ7+iw/OvHXa8Qwcb9L331LTbfkfpk3C6co" +
    "R/VCbZOUYamCTqe9KLI2N+vdLm8b45stDZ/wUsTfOL0B3sGOxYSZWcXOKejZWU9Vl7remU67/X5ja/sYH4Ehhjtw" +
    "VhCa0vcFNSfSsHhwRVFk5wkph8P8s37v+f7B/gtpV1yX4kqRU/kfxajG436WqUnCosh0HHsw2H39Cs1HB9vf0Z3B" +
    "WICloBlqOUghQSeTfhwbQWBybo9GG9gdINbnk62vt36sn6LtwgyhxWBLUIEc9Px8JQxNz2s5Tg8bI9iONDteTRre" +
    "r41JaKA1Rz2EnoAlUDPZVjsILNdto+fA5jAjUQ7CBridaem052ZMTsCIoCXQhD2h7fsNmbvpwQykjqgu1uBbqLey" +
    "Wua2gowgFZ8uRpqASnbDRyOAFcKosoqOLBniOkJLlJzXyqAZFjXZ2UzFPwAAAP//RKuAsQAAAAZJREFUAwD2LCmd" +
    "7sDtgwAAAABJRU5ErkJggg==",
  "base64",
);

const solid = (width: number, height: number, channels: 3 | 4, color: ReadonlyArray<number>) =>
  encodePng({
    width,
    height,
    channels,
    data: Buffer.from(
      Array.from({ length: width * height }, () => color.slice(0, channels)).flat(),
    ),
  });

const pixel = (image: ReturnType<typeof decodePng>, x: number, y: number) => [
  ...image.data.subarray(
    (y * image.width + x) * image.channels,
    (y * image.width + x + 1) * image.channels,
  ),
];

it("reads the pixels of a capture Chromium made", () => {
  const image = decodePng(CHROMIUM_CAPTURE);
  expect([image.width, image.height, image.channels]).toEqual([16, 16, 3]);
  expect(pixel(image, 0, 0)).toEqual([238, 208, 17]);
  expect(pixel(image, 15, 0)).toEqual([238, 17, 17]);
  expect(pixel(image, 9, 3)).toEqual([238, 62, 17]);
  expect(pixel(image, 7, 9)).toEqual([141, 115, 17]);
  expect(pixel(image, 0, 15)).toEqual([17, 17, 238]);
  expect(pixel(image, 15, 15)).toEqual([47, 209, 17]);
  expect(decodePng(encodePng(image)).data.equals(image.data)).toBe(true);
  expect(() => decodePng(Buffer.from("not a png"))).toThrow("not a PNG");
});

it("cuts a clip into tiles the page paints whole", () => {
  // A 1920x1080 page at 2 device pixels per CSS pixel, where 1430x1016 pixels are painted.
  const plan = tileClip({ x: 0, y: 0, width: 1920, height: 1080, scale: 1 }, 2, {
    width: 1430,
    height: 1016,
  });
  expect([plan.width, plan.height]).toEqual([3840, 2160]);
  expect(plan.tiles.map((tile) => [tile.left, tile.top])).toEqual([
    [0, 0],
    [1430, 0],
    [2860, 0],
    [0, 1016],
    [1430, 1016],
    [2860, 1016],
    [0, 2032],
    [1430, 2032],
    [2860, 2032],
  ]);
  expect(plan.tiles[0]?.clip).toEqual({ x: 0, y: 0, width: 715, height: 508, scale: 1 });
  expect(plan.tiles[8]?.clip).toEqual({ x: 1430, y: 1016, width: 490, height: 64, scale: 1 });

  // The clip's own scale counts: half the pixels per CSS pixel, twice the CSS pixels per tile.
  const scaled = tileClip({ x: 10, y: 20, width: 1920, height: 400, scale: 0.5 }, 2, {
    width: 1430,
    height: 1016,
  });
  expect([scaled.width, scaled.height]).toEqual([1920, 400]);
  expect(scaled.tiles.map((tile) => tile.clip)).toEqual([
    { x: 10, y: 20, width: 1430, height: 400, scale: 0.5 },
    { x: 1440, y: 20, width: 490, height: 400, scale: 0.5 },
  ]);

  // 1.25 pixels per CSS pixel: 401 CSS pixels fit, but 400 make whole pixels, so tiles meet.
  const fractional = tileClip({ x: 0, y: 0, width: 1000, height: 300, scale: 1 }, 1.25, {
    width: 502,
    height: 400,
  });
  expect(fractional.tiles.map((tile) => [tile.left, tile.clip.x, tile.clip.width])).toEqual([
    [0, 0, 400],
    [500, 400, 400],
    [1000, 800, 200],
  ]);

  // obu sizes a full page to its last drawn pixel, which is part of a page pixel at this ratio.
  const partPixel = tileClip({ x: 0, y: 0, width: 800, height: 1000.5, scale: 1 }, 2, {
    width: 1700,
    height: 800,
  });
  expect(partPixel.height).toBe(2001);
  expect(partPixel.tiles.map((tile) => [tile.top, tile.clip.height])).toEqual([
    [0, 400],
    [800, 400],
    [1600, 200.5],
  ]);

  // Refused before any of it is captured: a scale no whole step meets, too many pixels to
  // hold, and more tiles than one image is joined from.
  const refused = (
    clip: { readonly width: number; readonly height: number },
    ratio: number,
    painted: { readonly width: number; readonly height: number },
  ) => {
    try {
      tileClip({ x: 0, y: 0, scale: 1, ...clip }, ratio, painted);
      return "joined";
    } catch (cause) {
      return String(cause);
    }
  };
  expect(refused({ width: 3000, height: 200 }, 1.2345, { width: 700, height: 400 })).toContain(
    "cannot join a capture at 1.2345 image pixels per page pixel",
  );
  expect(refused({ width: 8192, height: 8192 }, 1, { width: 500, height: 300 })).toContain(
    "at most 40 megapixels in one image, and this one is 8192x8192",
  );
  expect(refused({ width: 4000, height: 4000 }, 1, { width: 200, height: 200 })).toContain(
    "at most 256 pieces",
  );

  // A clip the page paints whole is one tile, which the caller sends as it came.
  expect(
    tileClip({ x: 0, y: 0, width: 700, height: 500, scale: 1 }, 2, { width: 1430, height: 1016 })
      .tiles,
  ).toHaveLength(1);
});

it("joins tiles at their places, whether a tile has an alpha channel or not", async () => {
  const joined = decodePng(
    await joinTiles(5, 3, [
      { left: 0, top: 0, png: solid(3, 2, 3, [200, 10, 10]) },
      { left: 3, top: 0, png: solid(2, 2, 4, [10, 200, 10, 255]) },
      // One pixel taller and wider than its place: the image keeps its own size.
      { left: 0, top: 2, png: solid(6, 2, 3, [10, 10, 200]) },
    ]),
  );
  expect([joined.width, joined.height, joined.channels]).toEqual([5, 3, 4]);
  expect(pixel(joined, 0, 0)).toEqual([200, 10, 10, 255]);
  expect(pixel(joined, 2, 1)).toEqual([200, 10, 10, 255]);
  expect(pixel(joined, 3, 0)).toEqual([10, 200, 10, 255]);
  expect(pixel(joined, 4, 1)).toEqual([10, 200, 10, 255]);
  expect(pixel(joined, 0, 2)).toEqual([10, 10, 200, 255]);
  expect(pixel(joined, 4, 2)).toEqual([10, 10, 200, 255]);
});
