import { useState } from "react";
import type { SnapShotSource } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import "~/index.css";
import { render } from "../../../test/render";
import { ExpandedImageDialog } from "./ExpandedImageDialog";

const PIXEL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

const snapShotSource: SnapShotSource = {
  kind: "snap-shot",
  capturedAt: "2026-01-01T00:00:00.000Z",
  appName: "Finder",
  windowTitle: "Downloads",
};

const D2_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1174 920"><rect width="1174" height="920"/></svg>';

function ImagePreviewOpener() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open preview
      </button>
      {open && (
        <ExpandedImageDialog
          preview={{ images: [{ src: PIXEL, name: "shot.png", source: snapShotSource }], index: 0 }}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

describe("ExpandedImageDialog", () => {
  it.each([
    { opener: "mouse", focusVisible: false },
    { opener: "keyboard", focusVisible: true },
  ])("returns $opener focus to the opener on close", async (c) => {
    await using screen = await render(<ImagePreviewOpener />, {
      // The opener renders no links and reads no route.
      router: false,
    });
    const opener = screen.getByRole("button", { name: "Open preview" });
    if (c.opener === "mouse") {
      await userEvent.click(opener);
    } else {
      await userEvent.keyboard("{Tab}");
      await userEvent.keyboard("{Enter}");
    }
    const close = page.getByRole("button", { name: "Close image preview" });
    await expect.element(close).toHaveFocus();
    expect(close.element().matches(":focus-visible")).toBe(c.focusVisible);
    await userEvent.keyboard("{Escape}");
    await expect.element(opener).toHaveFocus();
    expect(opener.element().matches(":focus-visible")).toBe(c.focusVisible);
  });

  it("Close is the first Tab stop and its context menu is not the media's", async () => {
    await using screen = await render(
      <ExpandedImageDialog
        preview={{ images: [{ src: PIXEL, name: "shot.png", source: snapShotSource }], index: 0 }}
        onClose={vi.fn()}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const close = screen.getByRole("button", { name: "Close image preview" });
    await expect.element(close).toHaveFocus();
    const dialog = screen.getByRole("dialog").element();
    expect(dialog.querySelector("a[href], button, [tabindex='0']")).toBe(close.element());
    const contextMenu = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    close.element().dispatchEvent(contextMenu);
    expect(contextMenu.defaultPrevented).toBe(false);

    await userEvent.keyboard("{Tab}");
    await expect
      .element(screen.getByRole("button", { name: "No accessibility data" }))
      .toHaveFocus();
    await userEvent.keyboard("{Tab}");
    const viewer = screen.getByRole("region", { name: "shot.png, zoomable image" }).element();
    expect(document.activeElement?.contains(viewer)).toBe(true);
  });

  it("keeps Close clear of Next for a short wide image in a gallery", async () => {
    await page.viewport(1280, 800);
    const wide = `data:image/svg+xml,${encodeURIComponent(
      '<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="40"><rect width="1600" height="40"/></svg>',
    )}`;
    const onClose = vi.fn();
    await using screen = await render(
      <ExpandedImageDialog
        preview={{
          images: [
            { src: wide, name: "wide.svg" },
            { src: PIXEL, name: "shot.png" },
          ],
          index: 0,
        }}
        onClose={onClose}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const close = screen.getByRole("button", { name: "Close image preview" });
    const previous = screen.getByRole("button", { name: "Previous media" });
    const next = screen.getByRole("button", { name: "Next media" });
    await expect.element(next).toBeVisible();
    const image = screen.getByRole("img", { name: "wide.svg" }).element() as HTMLElement;
    await vi.waitFor(() => expect(image.style.width).not.toBe(""));
    await vi.waitFor(() => {
      const a = close.element().getBoundingClientRect();
      const b = next.element().getBoundingClientRect();
      expect(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top).toBe(
        true,
      );
    });
    const order = close.element().compareDocumentPosition(previous.element());
    expect(order & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    await userEvent.click(close);
    expect(onClose).toHaveBeenCalled();
  });

  it.each(["/work/pipeline.svg", "/work/diagram#v2.svg", "/work/diagram?v2.svg"])(
    "fits a markdown SVG without intrinsic size to the viewer: %s",
    async (path) => {
      const src = URL.createObjectURL(new Blob([D2_SVG], { type: "image/svg+xml" }));
      using _src = { [Symbol.dispose]: () => URL.revokeObjectURL(src) };
      await using screen = await render(
        <ExpandedImageDialog
          preview={{
            images: [
              {
                src,
                name: "Pipeline",
                actionsSource: {
                  kind: "image",
                  name: "Pipeline",
                  src,
                  reference: { kind: "file", path },
                },
              },
            ],
            index: 0,
          }}
          onClose={vi.fn()}
        />,
        // The dialog renders no links and reads no route.
        { router: false },
      );

      const viewer = screen.getByRole("region", { name: "Pipeline, zoomable image" });
      // The image fills the stage along its limiting side.
      await vi.waitFor(() => {
        const style = getComputedStyle(viewer.element());
        const stageWidth = Number.parseFloat(style.getPropertyValue("--media-width"));
        const stageHeight = Number.parseFloat(style.getPropertyValue("--media-height"));
        const { width, height } = viewer.element().getBoundingClientRect();
        expect(Math.max(width / stageWidth, height / stageHeight)).toBeCloseTo(1, 2);
      });
      await expect.element(screen.getByText("100%", { exact: true })).toBeVisible();
    },
  );

  it.each(["header", "footer"])("a click on the empty %s closes the viewer", async (row) => {
    const onClose = vi.fn();
    await using screen = await render(
      <ExpandedImageDialog
        preview={{ images: [{ src: PIXEL, name: "shot.png" }], index: 0 }}
        onClose={onClose}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const close = screen.getByRole("button", { name: "Close image preview" });
    const toolbar = screen.getByRole("toolbar", { name: "Media controls" });
    await expect.element(toolbar).toBeVisible();
    const anchor = (row === "header" ? close : toolbar).element().getBoundingClientRect();
    const target = document.elementFromPoint(anchor.left - 30, anchor.top + anchor.height / 2);
    if (!(target instanceof HTMLElement)) throw new Error("Nothing beside the controls");
    target.click();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("reads and reaches the actual size of a capture past the zoom ceiling, and refits a replaced image", async () => {
    const tall = (color: string) =>
      `data:image/svg+xml,${encodeURIComponent(
        `<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="40000"><rect width="1000" height="40000" fill="${color}"/></svg>`,
      )}`;
    const dialog = (src: string) => (
      <ExpandedImageDialog
        preview={{ images: [{ src, name: "tall.svg" }], index: 0 }}
        onClose={() => {}}
      />
    );
    // The dialog renders no links and reads no route.
    await using screen = await render(dialog(tall("red")), { router: false });

    await userEvent.click(screen.getByRole("button", { name: "Actual size" }));
    await expect.element(screen.getByText("100%", { exact: true })).toBeVisible();
    await expect.element(screen.getByText("100% zoom")).toBeInTheDocument();

    await screen.rerender(dialog(tall("blue")));
    await expect.element(screen.getByRole("button", { name: "Actual size" })).toBeEnabled();
  });

  it("Escape closes a popup opened inside the dialog before the dialog", async () => {
    const onClose = vi.fn();
    await using screen = await render(
      <ExpandedImageDialog
        preview={{ images: [{ src: PIXEL, name: "shot.png", source: snapShotSource }], index: 0 }}
        onClose={onClose}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );

    await userEvent.click(screen.getByRole("button", { name: "No accessibility data" }));
    await expect.element(screen.getByText("Accessibility data")).toBeVisible();

    await userEvent.keyboard("{Escape}");
    await expect.element(screen.getByText("Accessibility data")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();

    // Focus returns to the popup's trigger and opens its tooltip; leave it for a plain control.
    (screen.getByRole("button", { name: "Close image preview" }).element() as HTMLElement).focus();
    await userEvent.keyboard("{Escape}");
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("scrolling pans the image while Ctrl+wheel zooms around the pointer", async () => {
    const canvas = document.createElement("canvas");
    canvas.width = 1600;
    canvas.height = 1200;
    await using screen = await render(
      <ExpandedImageDialog
        preview={{ images: [{ src: canvas.toDataURL(), name: "large.png" }], index: 0 }}
        onClose={() => {}}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const viewportLocator = screen.getByRole("region", { name: "large.png, zoomable image" });
    const viewport = viewportLocator.element() as HTMLElement;
    const image = screen.getByRole("img", { name: "large.png" }).element() as HTMLElement;
    await vi.waitFor(() => expect(image.style.width).not.toBe(""));
    const fitWidth = image.getBoundingClientRect().width;
    const wheels: { ctrlKey: boolean; canceled: boolean }[] = [];
    const recordWheel = (event: WheelEvent) =>
      wheels.push({ ctrlKey: event.ctrlKey, canceled: event.defaultPrevented });
    document.addEventListener("wheel", recordWheel);
    using _wheelListener = {
      [Symbol.dispose]: () => document.removeEventListener("wheel", recordWheel),
    };
    // Wheels land on the viewport's center; this is the image fraction under it.
    const pointerFraction = () => {
      const viewportBounds = viewport.getBoundingClientRect();
      const imageBounds = image.getBoundingClientRect();
      return {
        x: (viewportBounds.left + viewportBounds.width / 2 - imageBounds.left) / imageBounds.width,
        y: (viewportBounds.top + viewportBounds.height / 2 - imageBounds.top) / imageBounds.height,
      };
    };
    const ctrlWheel = async (deltaY: number) => {
      await userEvent.keyboard("{Control>}");
      await userEvent.wheel(viewportLocator, { delta: { y: deltaY } });
      await userEvent.keyboard("{/Control}");
    };

    // Synthetic: macOS Chromium keeps a native wheel on an unscrollable viewport and scrolls it after the zoom.
    viewport.dispatchEvent(
      new WheelEvent("wheel", { deltaY: 120, bubbles: true, cancelable: true }),
    );
    expect(wheels.at(-1)).toEqual({ ctrlKey: false, canceled: false });
    expect(image.getBoundingClientRect().width).toBe(fitWidth);

    await ctrlWheel(-100);
    expect(wheels.at(-1)).toEqual({ ctrlKey: true, canceled: true });
    const zoomedWidth = image.getBoundingClientRect().width;
    expect(zoomedWidth).toBeCloseTo(fitWidth * Math.E, 0);

    const pan = async (delta: { x: number; y: number }) => {
      const { scrollLeft, scrollTop } = viewport;
      await userEvent.wheel(viewportLocator, { delta });
      await vi.waitFor(() => {
        expect(Math.sign(viewport.scrollLeft - scrollLeft)).toBe(Math.sign(delta.x));
        expect(Math.sign(viewport.scrollTop - scrollTop)).toBe(Math.sign(delta.y));
      });
    };
    await pan({ x: 60, y: 0 });
    await pan({ x: 0, y: 60 });
    await pan({ x: -40, y: -40 });
    expect(wheels.filter((wheel) => !wheel.ctrlKey).every((wheel) => !wheel.canceled)).toBe(true);
    expect(image.getBoundingClientRect().width).toBe(zoomedWidth);

    const anchor = pointerFraction();
    await ctrlWheel(-50);
    expect(image.getBoundingClientRect().width).toBeGreaterThan(zoomedWidth);
    expect(pointerFraction().x).toBeCloseTo(anchor.x, 2);
    expect(pointerFraction().y).toBeCloseTo(anchor.y, 2);
  });

  it("draws in place: the image, its caption and the controls stay put", async () => {
    const file = new File([await (await fetch(PIXEL)).blob()], "shot.png", { type: "image/png" });
    const save = vi.fn(async () => null);
    const onClose = vi.fn();
    await using screen = await render(
      <ExpandedImageDialog
        preview={{
          images: [
            {
              src: PIXEL,
              name: "shot.png",
              mimeType: "image/png",
              edit: { file, name: "shot.png", mimeType: "image/png", save },
            },
            { src: PIXEL, name: "other.png" },
          ],
          index: 0,
        }}
        onClose={onClose}
      />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const image = screen.getByRole("img", { name: "shot.png", exact: true });
    const caption = screen.getByText("1 / 2");
    await expect.element(image).toBeVisible();
    const rects = () =>
      [image, caption].map((locator) => locator.element().getBoundingClientRect().toJSON());
    const before = rects();

    await userEvent.click(screen.getByRole("button", { name: "Draw on image" }));
    await expect.element(screen.getByRole("button", { name: "Done" })).toHaveFocus();
    expect(rects()).toEqual(before);
    await expect
      .element(screen.getByRole("button", { name: "Draw on image" }))
      .toHaveAttribute("aria-pressed", "true");
    await expect.element(screen.getByRole("button", { name: "Next media" })).toBeEnabled();

    // The close button cancels the drawing and keeps the preview open.
    await userEvent.click(screen.getByRole("button", { name: "Cancel drawing" }));
    await expect.element(screen.getByRole("button", { name: "Done" })).not.toBeInTheDocument();
    expect(rects()).toEqual(before);
    expect(onClose).not.toHaveBeenCalled();
    expect(save).not.toHaveBeenCalled();
  });
  it("draws on a fitted image, with its tools clear of the header", async () => {
    await page.viewport(1440, 900);
    const canvas = document.createElement("canvas");
    canvas.width = 1600;
    canvas.height = 1000;
    const src = canvas.toDataURL("image/png");
    const name = `${"a-long-screenshot-name-".repeat(4)}.png`;
    const file = new File([await (await fetch(src)).blob()], name, { type: "image/png" });
    const image = {
      src,
      name,
      mimeType: "image/png",
      edit: { file, name, mimeType: "image/png", save: vi.fn(async () => null) },
    };
    await using screen = await render(
      <ExpandedImageDialog preview={{ images: [image, image], index: 0 }} onClose={vi.fn()} />,
      // The dialog renders no links and reads no route.
      { router: false },
    );
    const scale = screen.getByRole("toolbar", { name: "Media controls" }).getByText(/%$/);
    await expect.element(scale).toBeVisible();
    const fitted = scale.element().textContent ?? "";
    await userEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await expect.element(scale).not.toHaveTextContent(fitted);

    await userEvent.click(screen.getByRole("button", { name: "Draw on image" }));
    await expect.element(screen.getByRole("button", { name: "Done" })).toBeVisible();
    await expect.element(scale).toHaveTextContent(fitted);
    const tools = screen
      .getByRole("group", { name: "Drawing tool" })
      .element()
      .closest(".absolute");
    if (!tools) throw new Error("No drawing toolbar");
    const toolsRect = tools.getBoundingClientRect();
    const close = screen.getByRole("button", { name: "Cancel drawing" }).element();
    const caption = screen.getByText("1 / 2").element().parentElement;
    if (!caption) throw new Error("No caption");
    for (const element of [close, caption]) {
      const rect = element.getBoundingClientRect();
      const overlaps =
        rect.left < toolsRect.right &&
        toolsRect.left < rect.right &&
        rect.top < toolsRect.bottom &&
        toolsRect.top < rect.bottom;
      expect(overlaps && getComputedStyle(element).visibility === "visible").toBe(false);
    }
  });
});
