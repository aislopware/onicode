import { expect, it } from "vite-plus/test";

import { installRecordingCursor } from "./RecordingCursor.ts";

it("adopts a constructed stylesheet without injecting style elements or inline style attributes and removes it on dispose", () => {
  const beforeSheets = [...document.adoptedStyleSheets];
  const recording = installRecordingCursor(document, window);
  using _recording = { [Symbol.dispose]: () => recording.dispose() };

  expect(document.querySelectorAll("style")).toHaveLength(0);
  expect(document.querySelectorAll("[style]")).toHaveLength(0);
  expect(document.adoptedStyleSheets).toHaveLength(beforeSheets.length + 1);
  expect(document.querySelector("[data-onicode-recording-cursor]")).not.toBeNull();
  expect(document.querySelector("[data-onicode-recording-agent-cursor]")).not.toBeNull();

  recording.dispose();

  expect([...document.adoptedStyleSheets]).toEqual(beforeSheets);
  expect(document.querySelector("[data-onicode-recording-cursor]")).toBeNull();
  expect(document.querySelector("[data-onicode-recording-agent-cursor]")).toBeNull();
});

it("keeps the cursors hidden, inert and fixed against a more specific page rule", () => {
  const page = document.createElement("style");
  page.textContent =
    "html > div[aria-hidden] { display: flex; pointer-events: auto; position: absolute; visibility: hidden; opacity: 0; }";
  document.head.append(page);
  using _page = { [Symbol.dispose]: () => page.remove() };
  const recording = installRecordingCursor(document, window);
  using _recording = { [Symbol.dispose]: () => recording.dispose() };
  const human = document.querySelector<HTMLElement>("[data-onicode-recording-cursor]")!;
  const agent = document.querySelector<HTMLElement>("[data-onicode-recording-agent-cursor]")!;
  const drawn = (element: HTMLElement) => {
    const style = getComputedStyle(element);
    return [style.display, style.pointerEvents, style.position, style.visibility];
  };

  expect([drawn(human), drawn(agent)]).toEqual([
    ["none", "none", "fixed", "visible"],
    ["none", "none", "fixed", "visible"],
  ]);
  recording.setController("human", { x: 4, y: 4 });
  recording.move({ x: 8, y: 8 });
  expect([drawn(human), drawn(agent)]).toEqual([
    ["block", "none", "fixed", "visible"],
    ["block", "none", "fixed", "visible"],
  ]);
  expect([getComputedStyle(human).opacity, getComputedStyle(agent).opacity]).toEqual(["1", "1"]);
  recording.setController("agent");
  expect(drawn(human)).toEqual(["none", "none", "fixed", "visible"]);
});
