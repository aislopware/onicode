import { expect, it, vi } from "vite-plus/test";
import { render } from "../../../test/render";
import "~/index.css";

// Vite serves the worker module through its transform with an inline source map (6.8 MB for
// a 1.3 MB file), which dominated this test under CPU contention. A blob of the raw file is the
// same code without the dev-server cost.
vi.mock(import("pdfjs-dist/build/pdf.worker.min.mjs?url"), async () => {
  const { default: source } = await import("pdfjs-dist/build/pdf.worker.min.mjs?raw");
  return {
    default: URL.createObjectURL(new Blob([source], { type: "text/javascript" })),
  };
});

// Holds one page's Find text until the test opens it, so a scan can be observed mid-flight.
const findGate = vi.hoisted(() => ({ page: 0, open: () => {}, opened: Promise.resolve() }));
function setup(page: number) {
  findGate.page = page;
  findGate.opened = new Promise((resolve) => (findGate.open = resolve));
}
// Holds one page's load until the test opens it, so the page measures late.
const loadGate = vi.hoisted(() => ({ page: 0, opened: Promise.resolve() }));
function holdPageLoad(page: number) {
  let resolve = () => {};
  loadGate.page = page;
  loadGate.opened = new Promise<void>((done) => (resolve = done));
  const open = () => {
    loadGate.page = 0;
    resolve();
  };
  return { open, [Symbol.dispose]: open };
}
// Holds a document's close until the test releases it, and counts the opens that did not wait.
const closeGate = vi.hoisted(() => ({
  held: false,
  released: Promise.resolve(),
  release: () => {},
  reopening: () => {},
  racedOpens: 0,
}));
function holdClose() {
  closeGate.held = true;
  closeGate.racedOpens = 0;
  closeGate.released = new Promise((resolve) => {
    closeGate.release = () => {
      closeGate.held = false;
      resolve();
    };
  });
  return new Promise<void>((resolve) => (closeGate.reopening = resolve));
}
// Counts each page's loads and link annotation reads by page number.
const pageReads = vi.hoisted(() => ({
  loads: new Map<number, number>(),
  annotations: new Map<number, number>(),
  wrapped: new WeakSet<object>(),
}));
function countPageReads() {
  pageReads.loads.clear();
  pageReads.annotations.clear();
  return pageReads;
}
vi.mock(import("./pdfDocument"), async (importOriginal) => {
  const actual = await importOriginal();
  return {
    ...actual,
    pdfClosed: () => {
      closeGate.reopening();
      return actual.pdfClosed();
    },
    openPdf: (bytes) => {
      if (closeGate.held) {
        closeGate.racedOpens += 1;
        closeGate.reopening();
      }
      const task = actual.openPdf(bytes);
      const destroy = task.destroy.bind(task);
      task.destroy = async () => {
        await closeGate.released;
        return destroy();
      };
      const promise = task.promise.then((document) => {
        const getPage = document.getPage.bind(document);
        document.getPage = async (number) => {
          pageReads.loads.set(number, (pageReads.loads.get(number) ?? 0) + 1);
          if (number === loadGate.page) await loadGate.opened;
          const page = await getPage(number);
          const getTextContent = page.getTextContent.bind(page);
          page.getTextContent = async (params) => {
            if (number === findGate.page) await findGate.opened;
            return getTextContent(params);
          };
          if (!pageReads.wrapped.has(page)) {
            pageReads.wrapped.add(page);
            const getAnnotations = page.getAnnotations.bind(page);
            page.getAnnotations = (params) => {
              pageReads.annotations.set(number, (pageReads.annotations.get(number) ?? 0) + 1);
              return getAnnotations(params);
            };
          }
          return page;
        };
        return document;
      });
      return Object.defineProperty(task, "promise", { value: promise });
    },
  };
});

import PdfViewer from "./PdfViewer";

// Page 1 is a short banner whose title is drawn in two runs with a font change, so pdf.js
// splits it into two text spans. Page 2 is a letter page rotated a quarter turn.
function testPdf(
  pages: { box: string; rotate: number; text: string; annots?: string }[] = [
    {
      box: "0 0 612 140",
      rotate: 0,
      text: "BT /F1 24 Tf 30 70 Td (Quarter) Tj /F2 24 Tf (ly outlook) Tj ET",
    },
    { box: "0 0 612 792", rotate: 90, text: "BT /F1 24 Tf 30 700 Td (Budget and risks) Tj ET" },
  ],
) {
  const fonts = 3 + pages.length * 2;
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    `<< /Type /Pages /Count ${pages.length} /Kids [${pages.map((_, index) => `${3 + index * 2} 0 R`).join(" ")}] >>`,
    ...pages.flatMap(({ box, rotate, text, annots = "" }, index) => [
      `<< /Type /Page /Parent 2 0 R /MediaBox [${box}] /Rotate ${rotate}${annots && ` /Annots [${annots}]`} /Resources << /Font << /F1 ${fonts} 0 R /F2 ${fonts + 1} 0 R >> >> /Contents ${4 + index * 2} 0 R >>`,
      `<< /Length ${text.length} >>\nstream\n${text}\nendstream`,
    ]),
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>",
  ];
  let source = "%PDF-1.4\n";
  const offsets: number[] = [];
  for (const [index, object] of objects.entries()) {
    offsets.push(source.length);
    source += `${index + 1} 0 obj\n${object}\nendobj\n`;
  }
  const xref = source.length;
  source += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets
    .map((offset) => `${String(offset).padStart(10, "0")} 00000 n \n`)
    .join("")}trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF`;
  return new Blob([source], { type: "application/pdf" });
}

it("fits the width, then finds split and rotated text and jumps to its page", async () => {
  const url = URL.createObjectURL(testPdf());
  try {
    const container = document.createElement("div");
    container.style.cssText = "display:flex;width:500px;height:600px";
    await using screen = await render(<PdfViewer src={url} title="Planning PDF" />, {
      container,
    });
    // The milestone is pdf.js booting its worker and parsing the document, not a frame count.
    await expect.element(screen.getByText("1 / 2 pages"), { timeout: 10_000 }).toBeVisible();
    const region = screen.container.querySelector<HTMLElement>('[role="region"]')!;
    // Fit width follows the measured viewport, which settles once the scrollbar appears.
    await vi.waitFor(() => {
      const canvas = region.querySelector('canvas[aria-label="Page 1"]')!;
      expect(canvas.getBoundingClientRect().width).toBe(region.clientWidth - 32);
      expect(region.scrollWidth).toBe(region.clientWidth);
    });

    const find = screen.getByRole("searchbox", { name: "Find in PDF" });
    await find.fill("quarterly outlook");
    await expect.element(screen.getByRole("status")).toHaveTextContent("1/1");
    await vi.waitFor(() =>
      expect(
        Array.from(region.querySelectorAll(".pdf-find-match"), (span) => span.textContent),
      ).toEqual(["Quarter", "ly outlook"]),
    );

    await find.fill("budget");
    await expect.element(screen.getByText("2 / 2 pages")).toBeVisible();
    await expect.element(screen.getByRole("status")).toHaveTextContent("1/1");
    const rotated = await vi.waitFor(() => {
      const span = Array.from(region.querySelectorAll("[data-pdf-text-layer] span")).find(
        (node) => node.textContent === "Budget and risks",
      );
      expect(span).toBeDefined();
      return span!;
    });
    // The rotated page's text runs top to bottom inside the page, so a selection lands on it.
    const textBounds = rotated.getBoundingClientRect();
    const pageBounds = rotated.closest("[data-pdf-text-layer]")!.getBoundingClientRect();
    expect(textBounds.height).toBeGreaterThan(textBounds.width);
    expect(textBounds.width).toBeGreaterThan(0);
    expect(textBounds.left).toBeGreaterThanOrEqual(pageBounds.left - 1);
    expect(textBounds.top).toBeGreaterThanOrEqual(pageBounds.top - 1);
    expect(textBounds.right).toBeLessThanOrEqual(pageBounds.right + 1);
    expect(textBounds.bottom).toBeLessThanOrEqual(pageBounds.bottom + 1);
    expect(getComputedStyle(rotated).userSelect).toBe("text");
    const range = document.createRange();
    range.selectNodeContents(rotated);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
    expect(selection.toString()).toBe("Budget and risks");
    selection.removeAllRanges();

    await find.fill(" ");
    await expect.element(screen.getByRole("status")).toHaveTextContent("0/0");
  } finally {
    URL.revokeObjectURL(url);
  }
});

it("keeps the painted page while its width changes, then repaints it without a blank frame", async () => {
  const url = URL.createObjectURL(testPdf());
  try {
    const container = document.createElement("div");
    container.style.cssText = "display:flex;width:500px;height:600px";
    await using screen = await render(<PdfViewer src={url} title="Resized PDF" />, { container });
    await expect.element(screen.getByText("1 / 2 pages"), { timeout: 10_000 }).toBeVisible();
    const region = screen.container.querySelector<HTMLElement>('[role="region"]')!;
    const canvas = region.querySelector<HTMLCanvasElement>('canvas[aria-label="Page 1"]')!;
    const bitmapWidth = () =>
      Math.ceil((region.clientWidth - 32) * Math.min(2, window.devicePixelRatio));
    await vi.waitFor(() => expect(canvas.width).toBe(bitmapWidth()));
    const painted = canvas.width;

    // A canvas loses its pixels when its bitmap is resized, so read them as soon as that happens.
    const repainted = new Promise<number[]>((resolve) =>
      new MutationObserver(() =>
        resolve(Array.from(canvas.getContext("2d")!.getImageData(0, 0, 1, 1).data)),
      ).observe(canvas, { attributeFilter: ["width"] }),
    );
    const resized = new Promise((resolve) => new ResizeObserver(resolve).observe(container));
    container.style.width = "400px";
    await resized;
    expect(canvas.getBoundingClientRect().width).toBe(region.clientWidth - 32);
    expect(canvas.width).toBe(painted);

    expect(await repainted).toEqual([255, 255, 255, 255]);
    expect(canvas.width).toBe(bitmapWidth());
  } finally {
    URL.revokeObjectURL(url);
  }
});

it("keeps the page in view, with its painted canvas, when the pane narrows", async () => {
  const page = { box: "0 0 612 792", rotate: 0, text: "BT /F1 24 Tf 30 700 Td (Text) Tj ET" };
  const url = URL.createObjectURL(testPdf(Array.from({ length: 6 }, () => page)));
  try {
    const container = document.createElement("div");
    container.style.cssText = "display:flex;width:500px;height:300px";
    await using screen = await render(
      <PdfViewer
        src={url}
        title="Narrowed PDF"
        initialView={{ page: 3, offset: 100, zoom: null, left: 0 }}
      />,
      { container },
    );
    await expect.element(screen.getByText("4 / 6 pages"), { timeout: 10_000 }).toBeVisible();
    const region = screen.container.querySelector<HTMLElement>('[role="region"]')!;
    const canvas = region.querySelector<HTMLCanvasElement>('canvas[aria-label="Page 4"]')!;
    await vi.waitFor(() => expect(canvas.width).toBeGreaterThan(300));

    container.style.width = "300px";
    await vi.waitFor(() => {
      const top = region.getBoundingClientRect().top;
      const box = canvas.getBoundingClientRect();
      expect(box.width).toBe(region.clientWidth - 32);
      expect(box.top).toBeLessThanOrEqual(top);
      expect(box.bottom).toBeGreaterThan(top);
    });
    await expect.element(screen.getByText("4 / 6 pages")).toBeVisible();
  } finally {
    URL.revokeObjectURL(url);
  }
});

it("jumps to the first hit while still counting and keeps later manual navigation", async () => {
  setup(4);
  const page = (text: string) => ({
    box: "0 0 612 792",
    rotate: 0,
    text: `BT /F1 24 Tf 30 700 Td (${text}) Tj ET`,
  });
  const url = URL.createObjectURL(
    testPdf([page("Intro"), page("Needle one"), page("Needle two"), page("Closing")]),
  );
  try {
    await using screen = await render(
      <div className="flex h-[300px] w-[320px]">
        <PdfViewer src={url} title="Searched PDF" />
      </div>,
    );
    await expect.element(screen.getByText("1 / 4 pages")).toBeVisible();
    await screen.getByRole("searchbox", { name: "Find in PDF" }).fill("needle");
    await expect.element(screen.getByText("2 / 4 pages")).toBeVisible();
    await expect.element(screen.getByRole("status")).toHaveTextContent("Searching…");

    await screen.getByRole("button", { name: "Next page", exact: true }).click();
    await expect.element(screen.getByText("3 / 4 pages")).toBeVisible();
    findGate.open();
    await expect.element(screen.getByRole("status")).toHaveTextContent("1/2");
    await expect.element(screen.getByText("3 / 4 pages")).toBeVisible();
  } finally {
    findGate.open();
    URL.revokeObjectURL(url);
  }
});

it("restores page, within-page offset and zoom after a remount", async () => {
  const url = URL.createObjectURL(testPdf());
  const viewer = (mount: number, positionKey: string) => (
    <div className="flex h-[300px] w-[320px]">
      <PdfViewer key={mount} src={url} title="Restored PDF" positionKey={positionKey} />
    </div>
  );
  try {
    await using screen = await render(viewer(1, "restore:a"));
    await expect.element(screen.getByText("1 / 2 pages")).toBeVisible();
    await screen.getByRole("button", { name: "Zoom in", exact: true }).click();
    await screen.getByRole("button", { name: "Next page", exact: true }).click();
    await expect.element(screen.getByText("2 / 2 pages")).toBeVisible();
    const region = () => screen.getByRole("region", { name: "Restored PDF" }).element();
    const top = region().scrollTop + 40;
    const scrolled = new Promise((resolve) =>
      region().addEventListener("scroll", resolve, { once: true }),
    );
    region().scrollTop = top;
    await scrolled;
    await vi.waitFor(() => expect(region().scrollTop).toBeCloseTo(top, 0));
    const zoom = screen.container.textContent!.match(/\d+%/)![0];

    await screen.rerender(viewer(2, "restore:a"));
    await expect.element(screen.getByText("2 / 2 pages")).toBeVisible();
    await vi.waitFor(() => expect(region().scrollTop).toBeCloseTo(top, 0));
    await vi.waitFor(() => expect(screen.container.textContent).toContain(zoom));

    await screen.getByRole("button", { name: "Fit width", exact: true }).click();
    await vi.waitFor(() => {
      const canvas = region().querySelector("canvas")!;
      expect(canvas.getBoundingClientRect().width).toBe(region().clientWidth - 32);
    });
    const fitTop = region().scrollTop;
    await screen.rerender(viewer(3, "restore:a"));
    await expect.element(screen.getByText("2 / 2 pages")).toBeVisible();
    await vi.waitFor(() => {
      expect(region().querySelector("canvas")!.getBoundingClientRect().width).toBe(
        region().clientWidth - 32,
      );
      expect(region().scrollTop).toBeCloseTo(fitTop, 0);
    });

    // A reopen waits for the close before it: pdf.js frees the renderer only then, and an
    // open that finds it taken starts a second one.
    const reopening = holdClose();
    await screen.rerender(viewer(4, "restore:b"));
    await reopening;
    closeGate.release();
    await expect.element(screen.getByText("1 / 2 pages")).toBeVisible();
    await expect.element(screen.getByText("Fit width")).toBeVisible();
    expect(closeGate.racedOpens).toBe(0);
  } finally {
    closeGate.release();
    URL.revokeObjectURL(url);
  }
});

it("follows a link to a point on another page and reads links once across a zoom", async () => {
  const { loads, annotations: annotationCounts } = countPageReads();
  const page = (text: string) => ({
    box: "0 0 612 792",
    rotate: 0,
    text: `BT /F1 24 Tf 30 700 Td (${text}) Tj ET`,
  });
  const url = URL.createObjectURL(
    testPdf([
      {
        box: "0 0 612 792",
        rotate: 0,
        text: "BT /F1 24 Tf 30 700 Td (See p. 2) Tj 0 -60 Td (Website) Tj ET",
        annots: [
          // Page 4's object; y 200 is 2800 PDF units below its top edge.
          "<< /Type /Annot /Subtype /Link /Rect [30 690 200 730] /Border [0 0 0] /Dest [9 0 R /XYZ 0 200 0] >>",
          "<< /Type /Annot /Subtype /Link /Rect [30 630 200 670] /Border [0 0 0] /A << /S /URI /URI (https://example.com/) >> >>",
          // Page 6's top edge.
          "<< /Type /Annot /Subtype /Link /Rect [30 570 200 610] /Border [0 0 0] /Dest [13 0 R /XYZ 0 792 0] >>",
        ].join(" "),
      },
      page("Filler"),
      page("Filler"),
      // Offscreen and far taller than page 1, whose shape the layout assumes until it loads.
      { ...page("Target"), box: "0 0 612 3000" },
      // Taller than assumed, and mounted only once the reader reaches page 6 below it.
      { ...page("Taller"), box: "0 0 612 1600" },
      page("Linked"),
      // Taller than assumed, and loaded only once the test lets it.
      { ...page("After"), box: "0 0 612 1200" },
    ]),
  );
  try {
    const onOpenLink = vi.fn();
    await using screen = await render(
      <div className="flex h-[300px] w-[320px]">
        <PdfViewer src={url} title="Linked PDF" onOpenLink={onOpenLink} />
      </div>,
    );
    await expect.element(screen.getByText("1 / 7 pages")).toBeVisible();
    using lastPage = holdPageLoad(7);
    const region = screen.getByRole("region", { name: "Linked PDF" }).element();
    const web = await vi.waitFor(() => {
      const link = region.querySelector<HTMLAnchorElement>('a[href="https://example.com/"]');
      expect(link).not.toBeNull();
      return link!;
    });
    expect(getComputedStyle(web).cursor).toBe("pointer");
    web.dispatchEvent(new MouseEvent("auxclick", { button: 1, bubbles: true, cancelable: true }));
    expect(onOpenLink).toHaveBeenCalledWith("https://example.com/", expect.anything());
    const pageLink = (page: number) =>
      vi.waitFor(() => {
        const link = region.querySelector<HTMLAnchorElement>(`a[aria-label="Page ${page}"]`);
        expect(link).not.toBeNull();
        return link!;
      });
    await pageLink(4);

    // Link boxes are page-relative, so a zoom repaints the page without reading them again.
    const reads = annotationCounts.get(1);
    expect(reads).toBe(1);
    const canvas = region.querySelector<HTMLCanvasElement>('canvas[aria-label="Page 1"]')!;
    const painted = canvas.width;
    await screen.getByRole("button", { name: "Zoom in", exact: true }).click();
    await vi.waitFor(() => expect(canvas.width).not.toBe(painted));
    expect(annotationCounts.get(1)).toBe(reads);
    // A link's target page loads only when the link is followed.
    expect(loads.get(4)).toBeUndefined();

    // The jump holds while the page above it measures taller than assumed.
    (await pageLink(6)).click();
    await vi.waitFor(() => {
      const above = region.querySelector('canvas[aria-label="Page 5"]')!.getBoundingClientRect();
      const target = region.querySelector('canvas[aria-label="Page 6"]')!.getBoundingClientRect();
      expect(Math.abs(above.height - (1600 / 612) * above.width)).toBeLessThanOrEqual(1);
      expect(Math.abs(target.top - region.getBoundingClientRect().top)).toBeLessThanOrEqual(1);
    });
    await expect.element(screen.getByText("6 / 7 pages")).toBeVisible();
    // Next page hands the view back, even pressed with no pointer or key as assistive tech does:
    // page 7 measuring taller afterwards does not pull the view up to page 6.
    const next = screen.getByRole("button", { name: "Next page", exact: true }).element();
    (next as HTMLElement).click();
    const last = region.querySelector('canvas[aria-label="Page 7"]')!;
    lastPage.open();
    await vi.waitFor(() => {
      const measured = last.getBoundingClientRect();
      expect(Math.abs(measured.height - (1200 / 612) * measured.width)).toBeLessThanOrEqual(1);
    });
    const lastTop = last.getBoundingClientRect().top - region.getBoundingClientRect().top;
    expect(Math.abs(lastTop - 16)).toBeLessThanOrEqual(1);
    await expect.element(screen.getByText("7 / 7 pages")).toBeVisible();
    region.scrollTop = 0;
    await expect.element(screen.getByText("1 / 7 pages")).toBeVisible();
    (await pageLink(4)).click();
    await expect.element(screen.getByText("4 / 7 pages")).toBeVisible();
    await vi.waitFor(() => {
      const target = region.querySelector('canvas[aria-label="Page 4"]')!.getBoundingClientRect();
      const below = region.getBoundingClientRect().top - target.top;
      expect(Math.abs(below - (2800 / 612) * target.width)).toBeLessThanOrEqual(1);
    });
  } finally {
    URL.revokeObjectURL(url);
  }
});
