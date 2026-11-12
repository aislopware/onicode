import * as NodeVM from "node:vm";
import { describe, expect, it } from "vite-plus/test";

import { PREVIEW_STREAM_GLOBAL } from "../src/features/browser/preview-stream-document";
import { DEVICE_STREAM_GLOBAL } from "../src/features/devices/device-stream-document";
import { REMOTE_DESKTOP_GLOBAL } from "../src/features/remote-desktop/remote-desktop-document";
import { buildWebViewScript } from "./generate-device-stream.mts";

describe("stream WebView scripts", () => {
  it.each([
    ["device", DEVICE_STREAM_GLOBAL],
    ["preview", PREVIEW_STREAM_GLOBAL],
    ["remote-desktop", REMOTE_DESKTOP_GLOBAL],
  ] as const)("%s script defines the global its document starts", async (stream, global) => {
    const context: Record<string, unknown> = { TextEncoder, TextDecoder };
    NodeVM.runInNewContext(await buildWebViewScript(stream), context);
    expect(context[global]).toMatchObject({
      start: expect.any(Function),
      stop: expect.any(Function),
    });
  });
});
