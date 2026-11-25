/// <reference lib="dom" />
// Holds pasted-image compression open in the page, so a scenario can act while an image is
// still preparing, then tells it when every image decode, encode and read has settled.
import { expect } from "e2e";
import type { Browser } from "@e2e-dev/web";

interface CompressionHold {
  readonly release: () => void;
  readonly releaseNext: () => void;
  readonly held: Array<() => void>;
  released: boolean;
  releasedDecodes: number;
  compressionDecodes: number;
  inFlight: number;
}

declare global {
  interface Window {
    compressionHold?: CompressionHold;
  }
}

/** Installs the hold before the app loads; call before `onicode.open`. */
export const holdImageCompression = (browser: Browser) =>
  browser.addInitScript(() => {
    const hold: CompressionHold = {
      release: () => {
        hold.released = true;
        for (const resume of hold.held.splice(0)) resume();
      },
      releaseNext: () => {
        hold.releasedDecodes++;
        hold.held.shift()?.();
      },
      held: [],
      released: false,
      releasedDecodes: 0,
      compressionDecodes: 0,
      inFlight: 0,
    };
    window.compressionHold = hold;
    const track = async <T>(work: () => Promise<T>) => {
      hold.inFlight++;
      try {
        return await work();
      } finally {
        hold.inFlight--;
      }
    };

    const decode = window.createImageBitmap.bind(window);
    window.createImageBitmap = ((source: ImageBitmapSource, ...rest: [ImageBitmapOptions?]) =>
      track(async () => {
        // Only attachment compression decodes with an orientation; placeholder thumbnails do not.
        if (rest[0]?.imageOrientation === undefined) return decode(source, ...rest);
        if (!hold.released) await new Promise<void>((resume) => hold.held.push(resume));
        const bitmap = await decode(source, ...rest);
        hold.compressionDecodes++;
        return bitmap;
      })) as typeof createImageBitmap;

    const convertToBlob = OffscreenCanvas.prototype.convertToBlob;
    OffscreenCanvas.prototype.convertToBlob = function (options) {
      return track(() => convertToBlob.call(this, options));
    };
    const arrayBuffer = Blob.prototype.arrayBuffer;
    Blob.prototype.arrayBuffer = function () {
      return track(() => arrayBuffer.call(this));
    };
    const readAsDataURL = FileReader.prototype.readAsDataURL;
    FileReader.prototype.readAsDataURL = function (blob) {
      hold.inFlight++;
      this.addEventListener("loadend", () => hold.inFlight--, { once: true });
      readAsDataURL.call(this, blob);
    };

    const NativeWorker = window.Worker;
    if (typeof NativeWorker === "function") {
      window.Worker = new Proxy(NativeWorker, {
        construct(Target, args: [string | URL, WorkerOptions?]) {
          const worker = new Target(...args);
          if (!String(args[0]).includes("imageCompression.worker")) return worker;

          const pendingTasks = new Set<number>();
          const fitTasks = new Set<number>();
          const clearPending = () => {
            hold.inFlight -= pendingTasks.size;
            pendingTasks.clear();
            fitTasks.clear();
          };
          const postMessage = worker.postMessage.bind(worker) as (...postArgs: unknown[]) => void;
          const terminate = worker.terminate.bind(worker);

          worker.addEventListener("message", (event: MessageEvent<{ id?: unknown }>) => {
            const id = event.data?.id;
            if (typeof id !== "number" || !pendingTasks.delete(id)) return;
            if (fitTasks.delete(id)) hold.compressionDecodes++;
            hold.inFlight--;
          });
          worker.addEventListener("error", clearPending);

          worker.postMessage = ((...postArgs: unknown[]) => {
            const message = postArgs[0] as { id?: unknown; task?: { kind?: unknown } } | undefined;
            const id = message?.id;
            if (typeof id !== "number") {
              postMessage(...postArgs);
              return;
            }
            pendingTasks.add(id);
            hold.inFlight++;
            if (message?.task?.kind === "fit") {
              fitTasks.add(id);
              if (!hold.released) {
                hold.held.push(() => {
                  if (pendingTasks.has(id)) postMessage(...postArgs);
                });
                return;
              }
            }
            postMessage(...postArgs);
          }) as Worker["postMessage"];

          worker.terminate = () => {
            clearPending();
            terminate();
          };
          return worker;
        },
      });
    }
  });

/** Pastes a PNG wider than the 2000 px attachment limit into the focused composer. */
export const pasteWideScreenshot = (browser: Browser) =>
  browser.evaluate(async () => {
    const canvas = new OffscreenCanvas(2200, 20);
    canvas.getContext("2d")!.fillRect(0, 0, 2200, 20);
    const file = new File([await canvas.convertToBlob({ type: "image/png" })], "screenshot.png", {
      type: "image/png",
    });
    const clipboardData = new DataTransfer();
    clipboardData.items.add(file);
    const editor = document.querySelector('[contenteditable="true"]');
    if (!editor) throw new Error("No composer editor to paste into");
    return editor.dispatchEvent(
      new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
    );
  });

/**
 * Lets the held compression finish and waits until it has. What follows the last tracked step
 * is synchronous or microtask-only, so the app has applied the result by the next evaluate.
 */
export const finishImageCompression = async (browser: Browser) => {
  await browser.evaluate(() => {
    window.compressionHold!.release();
    return null;
  });
  await expect
    .poll(() =>
      browser.evaluate(() => {
        const hold = window.compressionHold!;
        return hold.released && hold.compressionDecodes > 0 && hold.inFlight === 0;
      }),
    )
    .toBe(true);
};

/** Waits until `count` compressions are held, in the order their images were pasted. */
export const expectHeldImageCompressions = (browser: Browser, count: number) =>
  expect.poll(() => browser.evaluate(() => window.compressionHold!.held.length)).toBe(count);

/** Lets the oldest held compression finish, keeping the rest held, and waits until it has. */
export const finishNextImageCompression = async (browser: Browser) => {
  await browser.evaluate(() => {
    window.compressionHold!.releaseNext();
    return null;
  });
  await expect
    .poll(() =>
      browser.evaluate(() => {
        const hold = window.compressionHold!;
        return (
          hold.compressionDecodes === hold.releasedDecodes && hold.inFlight === hold.held.length
        );
      }),
    )
    .toBe(true);
};
