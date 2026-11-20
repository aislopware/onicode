import { describe, expect, it } from "vite-plus/test";
import type { DesktopUpdateState } from "@onicode/contracts";

import {
  canCheckForUpdate,
  getDesktopUpdateButtonTooltip,
  getDesktopUpdateInstallConfirmationMessage,
  getDesktopUpdatePromptVersion,
  getDesktopUpdateReleaseHistoryUrl,
  getDesktopUpdateReleaseUrl,
  getRequiredDesktopUpdateView,
  resolveDesktopUpdateButtonAction,
} from "./desktopUpdate.logic";

const baseState: DesktopUpdateState = {
  enabled: true,
  status: "idle",
  currentVersion: "1.0.0",
  hostArch: "x64",
  appArch: "x64",
  runningUnderArm64Translation: false,
  availableVersion: null,
  downloadedVersion: null,
  releaseNotes: [],
  omittedReleaseCount: 0,
  downloadPercent: null,
  checkedAt: null,
  message: null,
  errorContext: null,
  canRetry: false,
};

describe("desktop update button state", () => {
  it("keeps retry action available after a download error", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "error",
      availableVersion: "1.1.0",
      message: "network timeout",
      errorContext: "download",
      canRetry: true,
    };
    expect(resolveDesktopUpdateButtonAction(state)).toBe("download");
    expect(getDesktopUpdateButtonTooltip(state)).toContain("Click to retry");
  });

  it("shows the download failure after returning to available", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "available",
      availableVersion: "1.1.0",
      message: "network timeout",
      errorContext: "download",
      canRetry: true,
    };
    expect(resolveDesktopUpdateButtonAction(state)).toBe("download");
    expect(getDesktopUpdateButtonTooltip(state)).toBe("Download failed for 1.1.0. Click to retry.");
    expect(getDesktopUpdateButtonTooltip({ ...state, notAppliedVersion: "1.1.0" })).toBe(
      "Update 1.1.0 did not apply. Click to retry.",
    );
  });

  it("prefers a newly available release over a stale downloaded version", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "available",
      availableVersion: "1.2.0",
      downloadedVersion: "1.1.0",
    };
    expect(resolveDesktopUpdateButtonAction(state)).toBe("download");
  });

  it("hides the install action while checking for a newer release", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "checking",
      availableVersion: "1.1.0",
      downloadedVersion: "1.1.0",
      downloadPercent: 100,
    };
    expect(resolveDesktopUpdateButtonAction(state)).toBe("none");
  });
});

describe("desktop update UI helpers", () => {
  it.each(["owner/repo/extra", "owner#notes/repo", "owner/repo?tab=notes"])(
    "falls back to upstream for an invalid configured repository: %s",
    (repository) => {
      expect(getDesktopUpdateReleaseHistoryUrl(repository)).toBe(
        "https://github.com/aislopware/onicode/releases",
      );
      expect(getDesktopUpdateReleaseUrl("1.2.3", repository)).toBe(
        "https://github.com/aislopware/onicode/releases/tag/v1.2.3",
      );
    },
  );
});

describe("canCheckForUpdate", () => {
  it("returns true once an update has been downloaded so newer releases can be found", () => {
    expect(
      canCheckForUpdate({
        ...baseState,
        status: "downloaded",
        availableVersion: "1.1.0",
        downloadedVersion: "1.1.0",
      }),
    ).toBe(true);
  });
});

describe("desktop update restart prompt", () => {
  const downloaded: DesktopUpdateState = {
    ...baseState,
    status: "downloaded",
    availableVersion: "1.1.0",
    downloadedVersion: "1.1.0",
    downloadPercent: 100,
  };
  const snooze = { version: "1.1.0", until: 1_000 };

  it.each([
    { name: "a downloaded update", state: downloaded, snooze: null, expected: "1.1.0" },
    { name: "a running snooze", state: downloaded, snooze, expected: null },
    {
      name: "a snooze that ran out",
      state: downloaded,
      snooze: { ...snooze, until: 500 },
      expected: "1.1.0",
    },
    {
      name: "a snooze for an older version",
      state: downloaded,
      snooze: { ...snooze, version: "1.0.9" },
      expected: "1.1.0",
    },
    {
      name: "an update still downloading",
      state: { ...downloaded, status: "downloading", downloadedVersion: null },
      snooze: null,
      expected: null,
    },
    {
      name: "an install that failed",
      state: { ...downloaded, errorContext: "install", message: "denied" },
      snooze: null,
      expected: null,
    },
    {
      name: "a version that did not apply",
      state: { ...downloaded, notAppliedVersion: "1.1.0" },
      snooze: null,
      expected: null,
    },
    {
      name: "a required update",
      state: { ...downloaded, required: true },
      snooze: null,
      expected: null,
    },
  ] as const)("offers $expected for $name", ({ state, snooze: active, expected }) => {
    expect(getDesktopUpdatePromptVersion(state, active, 500)).toBe(expected);
  });

  it("says whether running threads continue after the restart", () => {
    const on = getDesktopUpdateInstallConfirmationMessage(downloaded, true);
    const off = getDesktopUpdateInstallConfirmationMessage(downloaded, false);
    expect(on).toContain("1.1.0");
    expect(on).toContain("continue on their own");
    expect(off).toContain("stop until you message them again");
  });
});

describe("required desktop update", () => {
  const required: DesktopUpdateState = {
    ...baseState,
    status: "available",
    availableVersion: "1.1.0",
    required: true,
  };

  it.each([
    { name: "an optional update", state: { ...required, required: false }, expected: null },
    {
      name: "an update just found",
      state: required,
      expected: { phase: "downloading", version: "1.1.0", percent: null },
    },
    {
      name: "a download in progress",
      state: { ...required, status: "downloading", downloadPercent: 40 },
      expected: { phase: "downloading", version: "1.1.0", percent: 40 },
    },
    {
      name: "a downloaded update",
      state: { ...required, status: "downloaded", downloadedVersion: "1.1.0" },
      expected: { phase: "ready", version: "1.1.0" },
    },
    {
      name: "a failed download",
      state: { ...required, errorContext: "download", message: "offline" },
      expected: { phase: "failed", version: "1.1.0", retry: "download" },
    },
    {
      name: "a failed install",
      state: {
        ...required,
        status: "downloaded",
        downloadedVersion: "1.1.0",
        errorContext: "install",
        message: "denied",
      },
      expected: { phase: "failed", version: "1.1.0", retry: "install" },
    },
    {
      name: "a version that did not apply",
      state: {
        ...required,
        status: "downloaded",
        downloadedVersion: "1.1.0",
        notAppliedVersion: "1.1.0",
      },
      expected: { phase: "failed", version: "1.1.0", retry: "install" },
    },
    {
      name: "a failed check",
      state: { ...required, status: "error", errorContext: "check", message: "offline" },
      expected: { phase: "failed", version: "1.1.0", retry: "check" },
    },
  ] as const)("shows $name", ({ state, expected }) => {
    expect(getRequiredDesktopUpdateView(state)).toEqual(expected);
  });
});
