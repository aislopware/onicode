import { expect, it, vi } from "vite-plus/test";

const save = vi.hoisted(() => vi.fn<() => Promise<void>>());
vi.mock("expo", () => ({ requireNativeModule: () => ({ saveImageToLibrary: save }) }));
vi.mock("react-native", () => ({ Platform: { OS: "ios" } }));

import { saveImageToLibrary } from "./saveImageToLibrary";

it.each([
  ["ERR_PHOTOS_NOT_ALLOWED", "Allow Onicode to add photos in Settings."],
  ["ERR_PICTURES_SAVE_FAILED", "The image could not be saved to Pictures."],
  ["ERR_UNEXPECTED", "Native text"],
])("shows %s in the UI language, and system errors as they are", async (code, message) => {
  save.mockRejectedValueOnce(Object.assign(new Error("Native text"), { code }));
  await expect(saveImageToLibrary("file:///a.png", "image/png", "a.png")).rejects.toThrow(message);
});
