import { describe, expect, it } from "vite-plus/test";

import { makeMacInputTranslator } from "./macInput.ts";

// A second display to the right of a 1440-point-wide main display.
const BOUNDS = { x: 1440, y: 0, width: 1000, height: 500 };

describe("makeMacInputTranslator", () => {
  it("maps positions into the display and turns moves into drags while a button is down", () => {
    const input = makeMacInputTranslator();
    expect(input.translate({ type: "move", x: 0.5, y: 0.5 }, BOUNDS)).toEqual([
      { kind: "mouse", type: 5, x: 1940, y: 250, button: 0, clicks: 0, flags: 0 },
    ]);
    input.translate({ type: "button", button: 2, down: true, x: 0, y: 0, clicks: 1 }, BOUNDS);
    expect(input.translate({ type: "move", x: 1, y: 1 }, BOUNDS)).toMatchObject([
      { type: 7, x: 2440, y: 500, button: 1 },
    ]);
    // A repeated press is not a second mouse-down.
    expect(
      input.translate({ type: "button", button: 2, down: true, x: 0, y: 0, clicks: 1 }, BOUNDS),
    ).toEqual([]);
  });

  it("carries held modifiers on every event until both sides of a pair are released", () => {
    const input = makeMacInputTranslator();
    input.translate({ type: "key", code: "ShiftLeft", down: true }, BOUNDS);
    input.translate({ type: "key", code: "ShiftRight", down: true }, BOUNDS);
    input.translate({ type: "key", code: "MetaLeft", down: true }, BOUNDS);
    expect(input.translate({ type: "key", code: "KeyC", down: true }, BOUNDS)).toEqual([
      { kind: "key", keyCode: 0x08, down: true, flags: 0x120000 },
    ]);
    expect(input.translate({ type: "key", code: "ShiftLeft", down: false }, BOUNDS)).toMatchObject([
      { flags: 0x120000 },
    ]);
    expect(input.translate({ type: "key", code: "ShiftRight", down: false }, BOUNDS)).toMatchObject(
      [{ flags: 0x100000 }],
    );
    expect(input.translate({ type: "scroll", dx: 4, dy: -30 }, BOUNDS)).toEqual([
      { kind: "scroll", dx: -4, dy: 30, flags: 0x100000 },
    ]);
  });

  it("ignores keys it cannot place and releases it never saw pressed", () => {
    const input = makeMacInputTranslator();
    expect(input.translate({ type: "key", code: "BrowserBack", down: true }, BOUNDS)).toEqual([]);
    expect(input.translate({ type: "key", code: "KeyA", down: false }, BOUNDS)).toEqual([]);
  });

  it("splits text into events of at most 20 units without cutting a surrogate pair", () => {
    const input = makeMacInputTranslator();
    const text = `${"a".repeat(19)}😀b`;
    expect(input.translate({ type: "text", text }, BOUNDS)).toEqual([
      { kind: "text", text: "a".repeat(19) },
      { kind: "text", text: "😀b" },
    ]);
  });

  it("releases held buttons, then keys, then modifiers", () => {
    const input = makeMacInputTranslator();
    input.translate({ type: "key", code: "MetaLeft", down: true }, BOUNDS);
    input.translate({ type: "key", code: "KeyA", down: true }, BOUNDS);
    input.translate({ type: "button", button: 0, down: true, x: 0.1, y: 0.2, clicks: 1 }, BOUNDS);
    expect(input.release()).toEqual([
      { kind: "mouse", type: 2, x: 1540, y: 100, button: 0, clicks: 1, flags: 0x100000 },
      { kind: "key", keyCode: 0x00, down: false, flags: 0x100000 },
      { kind: "key", keyCode: 0x37, down: false, flags: 0 },
    ]);
    expect(input.release()).toEqual([]);
  });
});
