import { describe, expect, it } from "vite-plus/test";

import { downscaleFor, preferredVideoCodecs } from "./host";

const codec = (mimeType: string, sdpFmtpLine?: string): RTCRtpCodec => ({
  mimeType,
  clockRate: 90000,
  ...(sdpFmtpLine === undefined ? {} : { sdpFmtpLine }),
});

describe("preferredVideoCodecs", () => {
  it("orders main-profile HEVC, then real-time H.264 from the highest profile down", () => {
    const high = codec(
      "video/H264",
      "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f",
    );
    const baseline = codec(
      "video/H264",
      "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
    );
    // WebKit leaves profile-id out, which means Main.
    const hevc = codec("video/H265", "level-id=93;tx-mode=SRST");
    expect(
      preferredVideoCodecs([
        codec("video/VP8"),
        codec(
          "video/H264",
          "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=640c1f",
        ),
        baseline,
        high,
        // Main 10: a viewer may decode only Main.
        codec("video/H265", "level-id=180;profile-id=2;tier-flag=0;tx-mode=SRST"),
        hevc,
        codec("video/rtx", "apt=96"),
      ]),
    ).toEqual([hevc, high, baseline]);
  });

  it("leaves the choice to the browser when the viewer offered neither HEVC nor H.264", () => {
    expect(preferredVideoCodecs([codec("video/VP8"), codec("video/AV1")])).toEqual([]);
  });
});

describe("downscaleFor", () => {
  it.each([
    // A phone showing a 5K display letterboxed: the width is what limits it.
    [{ width: 5120, height: 2880 }, { width: 1280, height: 2400 }, 4],
    // A viewer larger than the display never upscales on the host.
    [{ width: 1920, height: 1080 }, { width: 3840, height: 2160 }, 1],
    [{ width: 3024, height: 1964 }, { width: 2016, height: 1964 }, 1.5],
    [{ width: 1920, height: 1080 }, { width: 0, height: 0 }, 1],
  ])("fits %o into %o by %d", (source, viewport, expected) => {
    expect(downscaleFor(source, viewport)).toBe(expected);
  });
});
