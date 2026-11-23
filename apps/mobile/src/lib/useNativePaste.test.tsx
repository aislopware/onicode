import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";
import { useNativePaste } from "./useNativePaste";

function setup() {
  const root = createRoot(document.createElement("div"));
  let paste: ReturnType<typeof useNativePaste> = {};
  const holds: Array<ReadonlyArray<string> | "holding"> = [];
  const startPaste = vi.fn((_images: number) => {
    const index = holds.push("holding") - 1;
    return (uris: ReadonlyArray<string>) => {
      holds[index] = uris;
    };
  });
  function Probe() {
    const current = useNativePaste(startPaste);
    useLayoutEffect(() => {
      paste = current;
    });
    return null;
  }
  act(() => root.render(<Probe />));
  return {
    holds,
    startPaste,
    paste: () => paste,
    unmount: () => act(() => root.unmount()),
  };
}

it("holds from an image read's start until its paste event lands the files", () => {
  const harness = setup();

  harness.paste().onImageReadStart?.({ readId: 1, images: 3 });
  // Native builds before the count announce none.
  harness.paste().onImageReadStart?.({ readId: 2 });
  expect(harness.holds).toEqual(["holding", "holding"]);
  expect(harness.startPaste.mock.calls).toEqual([[3], [1]]);

  harness.paste().onPaste?.({ type: "unsupported", readId: 2 });
  harness.paste().onPaste?.({ type: "images", uris: ["file:///a.png"], readId: 1 });
  expect(harness.holds).toEqual([["file:///a.png"], []]);
  expect(harness.startPaste).toHaveBeenCalledTimes(2);

  // Android, and iOS text pastes, announce nothing; images start and land at once.
  harness.paste().onPaste?.({ type: "images", uris: ["file:///b.png", "file:///c.png"] });
  harness.paste().onPaste?.({ type: "unsupported" });
  harness.paste().onPaste?.({ type: "text", value: "b" });
  expect(harness.holds).toEqual([["file:///a.png"], [], ["file:///b.png", "file:///c.png"]]);
  expect(harness.startPaste).toHaveBeenLastCalledWith(2);

  harness.paste().onImageReadStart?.({ readId: 3 });
  harness.unmount();
  expect(harness.holds.at(-1)).toEqual([]);
});
