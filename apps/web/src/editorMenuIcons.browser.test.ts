import { describe, expect, it } from "vite-plus/test";

import { editorMenuIcons } from "./editorMenuIcons";

function foregroundColor(color: string): Disposable {
  const style = document.createElement("style");
  style.textContent = `.text-foreground { color: ${color}; }`;
  document.head.append(style);
  return { [Symbol.dispose]: () => style.remove() };
}

async function opaquePixels(dataUrl: string): Promise<number> {
  const image = new Image();
  image.src = dataUrl;
  await image.decode();
  const canvas = document.createElement("canvas");
  canvas.width = image.width;
  canvas.height = image.height;
  const context = canvas.getContext("2d")!;
  context.drawImage(image, 0, 0);
  const { data } = context.getImageData(0, 0, canvas.width, canvas.height);
  let opaque = 0;
  for (let alpha = 3; alpha < data.length; alpha += 4) if (data[alpha]! > 0) opaque += 1;
  return opaque;
}

describe("editorMenuIcons", () => {
  it("keeps a logo painted by a gradient", async () => {
    const icon = (await editorMenuIcons(["vscodium"])).get("vscodium");

    expect(icon).toMatch(/^data:image\/png;base64,/);
    expect(await opaquePixels(icon!)).toBeGreaterThan(0);
  });

  it("rasterizes again when a same-polarity theme changes currentColor", async () => {
    let first: string | undefined;
    {
      using _red = foregroundColor("rgb(255, 0, 0)");
      first = (await editorMenuIcons(["zed"])).get("zed");
    }
    using _blue = foregroundColor("rgb(0, 0, 255)");
    const second = (await editorMenuIcons(["zed"])).get("zed");

    expect(first).toMatch(/^data:image\/png;base64,/);
    expect(second).toMatch(/^data:image\/png;base64,/);
    expect(second).not.toBe(first);
  });
});
