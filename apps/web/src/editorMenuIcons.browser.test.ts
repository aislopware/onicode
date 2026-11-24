import { describe, expect, it } from "vite-plus/test";

import { editorMenuIcons } from "./editorMenuIcons";

function foregroundColor(color: string): Disposable {
  const style = document.createElement("style");
  style.textContent = `.text-foreground { color: ${color}; }`;
  document.head.append(style);
  return { [Symbol.dispose]: () => style.remove() };
}

describe("editorMenuIcons", () => {
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
