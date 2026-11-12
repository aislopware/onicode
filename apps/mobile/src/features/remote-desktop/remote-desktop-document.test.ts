import * as NodeVM from "node:vm";
import { describe, expect, it, vi } from "vite-plus/test";

import { remoteDesktopDocument, remoteDesktopMessage } from "./remote-desktop-document";

describe("native remote desktop document", () => {
  it("starts the script with its configuration and reports a bootstrap exception", () => {
    const postMessage = vi.fn();
    const started: unknown[] = [];
    const html = remoteDesktopDocument(
      JSON.stringify({ control: true, note: "</script><script>alert(1)</script>" }),
      'var OniRemoteDesktop={start(input){started.push(input);throw new Error("startup")}};',
    );
    expect(html.match(/<script>/g)).toHaveLength(1);
    NodeVM.runInNewContext(html.match(/<script>([\s\S]*)<\/script>/)![1]!, {
      started,
      window: { addEventListener() {}, ReactNativeWebView: { postMessage } },
    });
    expect(started).toEqual([{ control: true, note: "</script><script>alert(1)</script>" }]);
    expect(JSON.parse(postMessage.mock.calls[0]![0] as string)).toMatchObject({
      type: "status",
      status: "error",
    });
  });
});

describe("native remote desktop messages", () => {
  it.each([
    ['{"type":"offer","sdp":"v=0"}', { type: "offer", sdp: "v=0" }],
    ['{"type":"status","status":"streaming"}', { type: "status", status: "streaming" }],
    [
      '{"type":"status","status":"error","detail":"The host refused."}',
      { type: "status", status: "error", detail: "The host refused." },
    ],
    ['{"type":"modifiersUsed"}', { type: "modifiersUsed" }],
    ['{"type":"status","status":"reconnecting"}', { type: "status", status: "reconnecting" }],
    ['{"type":"clipboard","text":"copied"}', { type: "clipboard", text: "copied" }],
  ])("reads %s", (data, expected) => expect(remoteDesktopMessage(data)).toEqual(expected));

  it.each([
    "invalid JSON",
    "null",
    '"offer"',
    "{}",
    '{"type":"unknown"}',
    '{"type":"offer"}',
    '{"type":"offer","sdp":42}',
    '{"type":"offer","sdp":""}',
    '{"type":"status"}',
    '{"type":"status","status":"unknown"}',
    '{"type":"status","status":"error","detail":42}',
    '{"type":"clipboard"}',
    '{"type":"clipboard","text":""}',
    JSON.stringify({ type: "clipboard", text: "a".repeat(64_001) }),
  ])("ignores invalid bridge messages: %s", (data) =>
    expect(remoteDesktopMessage(data)).toBeNull(),
  );

  it("ignores an offer longer than the server accepts", () => {
    const sdp = "a".repeat(64 * 1024);
    expect(remoteDesktopMessage(JSON.stringify({ type: "offer", sdp }))).toEqual({
      type: "offer",
      sdp,
    });
    expect(remoteDesktopMessage(JSON.stringify({ type: "offer", sdp: `${sdp}a` }))).toBeNull();
  });
});
