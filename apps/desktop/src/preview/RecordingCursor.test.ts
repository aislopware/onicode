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
