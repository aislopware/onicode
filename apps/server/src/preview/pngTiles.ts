// @effect-diagnostics nodeBuiltinImport:off - zlib is the PNG codec; Effect has no wrapper for it.
import * as NodeUtil from "node:util";
import * as NodeZlib from "node:zlib";

/**
 * A capture assembled from tiles. A desktop tab is a webview inside the app window, and Chromium
 * paints only as much of one as that window could show: a capture larger than that comes back
 * with the page repeated across it, while each tile within it comes back whole.
 */
export interface CaptureClip {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly scale: number;
}

interface Pixels {
  readonly width: number;
  readonly height: number;
  /** 3 for RGB, 4 for RGBA; one byte each. */
  readonly channels: number;
  readonly data: Buffer;
}

const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

const paeth = (left: number, up: number, upLeft: number) => {
  const estimate = left + up - upLeft;
  const toLeft = Math.abs(estimate - left);
  const toUp = Math.abs(estimate - up);
  const toUpLeft = Math.abs(estimate - upLeft);
  return toLeft <= toUp && toLeft <= toUpLeft ? left : toUp <= toUpLeft ? up : upLeft;
};

/** Reads what Chromium writes: 8-bit RGB or RGBA, not interlaced. */
export const decodePng = (png: Buffer): Pixels => {
  if (!png.subarray(0, 8).equals(SIGNATURE)) throw new Error("The capture is not a PNG.");
  let header: Buffer | undefined;
  const compressed: Array<Buffer> = [];
  for (let offset = 8; offset + 12 <= png.length;) {
    const length = png.readUInt32BE(offset);
    const type = png.toString("latin1", offset + 4, offset + 8);
    const body = png.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") header = body;
    if (type === "IDAT") compressed.push(body);
    if (type === "IEND") break;
    offset += 12 + length;
  }
  const colorType = header?.[9];
  if (
    header === undefined ||
    header[8] !== 8 ||
    header[12] !== 0 ||
    (colorType !== 2 && colorType !== 6)
  ) {
    throw new Error("The capture is a PNG of a kind the panel cannot join.");
  }
  const width = header.readUInt32BE(0);
  const height = header.readUInt32BE(4);
  const channels = colorType === 6 ? 4 : 3;
  const stride = width * channels;
  const filtered = NodeZlib.inflateSync(Buffer.concat(compressed));
  const data = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y += 1) {
    const filter = filtered[y * (stride + 1)];
    const from = y * (stride + 1) + 1;
    const row = y * stride;
    for (let i = 0; i < stride; i += 1) {
      const left = i >= channels ? data[row + i - channels]! : 0;
      const up = y > 0 ? data[row - stride + i]! : 0;
      const upLeft = y > 0 && i >= channels ? data[row - stride + i - channels]! : 0;
      const predicted =
        filter === 1
          ? left
          : filter === 2
            ? up
            : filter === 3
              ? (left + up) >> 1
              : filter === 4
                ? paeth(left, up, upLeft)
                : 0;
      data[row + i] = (filtered[from + i]! + predicted) & 0xff;
    }
  }
  return { width, height, channels, data };
};

const chunk = (type: string, body: Buffer) => {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(body.length, 0);
  head.write(type, 4, "latin1");
  const check = Buffer.alloc(4);
  check.writeUInt32BE(NodeZlib.crc32(Buffer.concat([head.subarray(4), body])), 0);
  return Buffer.concat([head, body, check]);
};

/** A PNG file around scanlines that are already compressed. */
const pngFile = (width: number, height: number, channels: number, compressed: Buffer) => {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = channels === 4 ? 6 : 2;
  return Buffer.concat([
    SIGNATURE,
    chunk("IHDR", header),
    chunk("IDAT", compressed),
    chunk("IEND", Buffer.alloc(0)),
  ]);
};

export const encodePng = ({ width, height, channels, data }: Pixels) => {
  const stride = width * channels;
  const rows = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y += 1) {
    data.copy(rows, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  return pngFile(width, height, channels, NodeZlib.deflateSync(rows, { level: 4 }));
};

/**
 * The largest image the server joins: its pixels are held whole, at four bytes each. obu cuts
 * a capture to the same area, so one it sends is never refused here for its size.
 */
const MAX_JOINED_PIXELS = 40_000_000;

/** The most tiles one image is joined from: each is a capture of its own. */
const MAX_TILES = 256;

/**
 * The tiles of a clip, each no larger in output pixels than `painted`. `ratio` is the page's
 * device pixels per CSS pixel. A tile steps by whole CSS pixels that are whole output pixels
 * too, so tiles meet exactly and none overlaps another. A clip that needs tiles and has no such
 * step, or too many pixels or tiles, is refused before any of it is captured.
 */
export const tileClip = (
  clip: CaptureClip,
  ratio: number,
  painted: { readonly width: number; readonly height: number },
) => {
  const perCss = clip.scale * ratio;
  const width = Math.max(1, Math.round(clip.width * perCss));
  const height = Math.max(1, Math.round(clip.height * perCss));
  const along = (limit: number, length: number) => {
    const most = Math.max(1, Math.floor(limit / perCss));
    if (length <= most) return length;
    for (let step = most; step >= 1; step -= 1) {
      if (Math.abs(step * perCss - Math.round(step * perCss)) < 1e-6) return step;
    }
    return null;
  };
  const across = along(painted.width, clip.width);
  const down = along(painted.height, clip.height);
  if (across === clip.width && down === clip.height) {
    return { width, height, tiles: [{ left: 0, top: 0, clip }] };
  }
  if (across === null || down === null) {
    throw new Error(
      `The Browser panel cannot join a capture at ${Number(perCss.toFixed(4))} image pixels per page pixel: no whole number of page pixels is a whole number of image pixels. Capture at a scale such as 1, 1.5 or 2, or a size the Onicode window can show whole.`,
    );
  }
  if (width * height > MAX_JOINED_PIXELS) {
    throw new Error(
      `The Browser panel captures at most ${MAX_JOINED_PIXELS / 1_000_000} megapixels in one image, and this one is ${width}x${height}. Capture a smaller size or scale.`,
    );
  }
  if (Math.ceil(clip.width / across) * Math.ceil(clip.height / down) > MAX_TILES) {
    throw new Error(
      `The Browser panel joins at most ${MAX_TILES} pieces into one image, and this ${width}x${height} one needs more. Capture a smaller size, or make the Onicode window larger.`,
    );
  }
  const tiles: Array<{ readonly left: number; readonly top: number; readonly clip: CaptureClip }> =
    [];
  for (let y = 0; y < clip.height; y += down) {
    for (let x = 0; x < clip.width; x += across) {
      tiles.push({
        left: Math.round(x * perCss),
        top: Math.round(y * perCss),
        clip: {
          x: clip.x + x,
          y: clip.y + y,
          width: Math.min(across, clip.width - x),
          height: Math.min(down, clip.height - y),
          scale: clip.scale,
        },
      });
    }
  }
  return { width, height, tiles };
};

const deflate = NodeUtil.promisify(NodeZlib.deflate);

/**
 * One opaque image of `width` by `height` with each tile's pixels at its place. Tiles land
 * straight in the PNG's scanlines and are compressed off the event loop: a large capture must
 * not hold the server's other work.
 */
export const joinTiles = async (
  width: number,
  height: number,
  tiles: ReadonlyArray<{ readonly left: number; readonly top: number; readonly png: Buffer }>,
) => {
  const stride = width * 4 + 1;
  const scanlines = Buffer.alloc(stride * height, 0xff);
  for (let y = 0; y < height; y += 1) scanlines[y * stride] = 0;
  for (const tile of tiles) {
    const pixels = decodePng(tile.png);
    const columns = Math.min(pixels.width, width - tile.left);
    const rows = Math.min(pixels.height, height - tile.top);
    for (let y = 0; y < rows; y += 1) {
      const from = y * pixels.width * pixels.channels;
      const to = (tile.top + y) * stride + 1 + tile.left * 4;
      if (pixels.channels === 4) {
        pixels.data.copy(scanlines, to, from, from + columns * 4);
        continue;
      }
      for (let x = 0; x < columns; x += 1) {
        pixels.data.copy(scanlines, to + x * 4, from + x * 3, from + x * 3 + 3);
        scanlines[to + x * 4 + 3] = 0xff;
      }
    }
  }
  return pngFile(width, height, 4, await deflate(scanlines, { level: 4 }));
};
