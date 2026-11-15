import { describe, expect, it } from "vite-plus/test";

import { forgetSentImages, rememberSentImage, sentImageHandoff } from "./sentImageHandoff";

describe("sentImageHandoff", () => {
  it("does not hand an edited queued message's old image to its replacement", () => {
    const photoA = { uri: "file:///a.jpg", aspectRatio: 4 / 3 };
    rememberSentImage("message-1", 0, "a.jpg", photoA);
    expect(sentImageHandoff("message-1", 0, "a.jpg")).toEqual(photoA);
    expect(sentImageHandoff("message-1", 0, "b.jpg")).toBeUndefined();

    forgetSentImages("message-1");
    expect(sentImageHandoff("message-1", 0, "a.jpg")).toBeUndefined();
  });
});
