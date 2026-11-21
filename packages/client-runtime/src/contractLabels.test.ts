import { mFallback } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";
import { expect, it } from "vite-plus/test";

import { projectCloneProgressSummary } from "./contractLabels.ts";

it("translates Git LFS clone progress from its bytes, not the server's English detail", () => {
  const resolve = ((message, options) => `[fr] ${mFallback(message, options)}`) as MFunctionType;
  const clone = {
    stage: "checkout",
    percent: 50,
    detail: "Downloading Git LFS objects: 1.5 MiB in progress",
    lfsDownloadBytes: 1536 * 1024,
  } as const;
  expect(projectCloneProgressSummary(clone, resolve)).toBe(
    "[fr] Checking out files · 50% · [fr] Downloading Git LFS objects: 1.5 MiB in progress",
  );
  expect(projectCloneProgressSummary({ ...clone, lfsDownloadBytes: null }, resolve)).toBe(
    "[fr] Checking out files · 50% · Downloading Git LFS objects: 1.5 MiB in progress",
  );
});
