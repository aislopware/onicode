import type { DisplayList } from "@betteroffice/xlsx";
import { expect, it, vi } from "vite-plus/test";

const painted = vi.hoisted(() => [] as Array<{ x: number; commands: readonly unknown[] }>);
vi.mock("@betteroffice/xlsx", () => ({
  paintDisplayList: (_painter: unknown, list: DisplayList, _ratio: number, at: { x: number }) => {
    painted.push({ x: at.x, commands: list.commands });
  },
}));

const { paintFrame } = await import("./sheet-frame");

const fill = (x: number, w = 20): DisplayList["commands"][number] => ({
  op: "fillRect",
  x,
  y: 10,
  w,
  h: 20,
  color: "#000",
});

it("paints only the commands that can show in each band of a scrolled sheet", () => {
  painted.length = 0;
  const inPane = fill(10);
  const behindPane = fill(100);
  const visible = fill(1100);
  const pastViewport = fill(1400);
  const line: DisplayList["commands"][number] = {
    op: "line",
    x1: 1000,
    y1: 0,
    x2: 1300,
    y2: 0,
    width: 1,
    color: "#000",
  };
  const commands = [inPane, behindPane, visible, pastViewport, line];
  const list: DisplayList = { width: 2000, height: 100, commands };
  const context = {
    canvas: { width: 300, height: 100 },
    save: () => {},
    restore: () => {},
    setTransform: () => {},
    beginPath: () => {},
    rect: () => {},
    clip: () => {},
  } as unknown as OffscreenCanvasRenderingContext2D;

  // A 50 px frozen pane, then cells scrolled 1000 px behind it, in a 300 px viewport.
  paintFrame(
    context,
    {
      list,
      tiles: [{ list, x: 0, y: 0, clipX: 0, clipY: 0 }],
      viewport: { x: 1000, y: 0, width: 300, height: 100 },
      behind: { x: 1000, y: 0 },
      panes: { width: 50, height: 0 },
    },
    { ratio: 1, origin: { x: 0, y: 0 } },
  );

  expect(painted).toEqual([
    { x: 0, commands: [inPane] },
    { x: -1000, commands: [visible, line] },
  ]);
});
