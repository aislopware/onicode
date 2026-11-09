import { describe, expect, it } from "vite-plus/test";

import { cliArchiveTarCommand, newestCliReleaseVersion, parseChecksums } from "./cliRelease.ts";

describe("cliRelease", () => {
  it("parses sha256sum output including binary-mode markers", () => {
    const checksums = parseChecksums(
      [
        `${"a".repeat(64)}  oni-1.2.3-linux-x64.tar.gz`,
        `${"B".repeat(64)} *oni-1.2.3-win32-x64.zip`,
        "not a checksum line",
        "",
      ].join("\n"),
    );
    expect(checksums.get("oni-1.2.3-linux-x64.tar.gz")).toBe("a".repeat(64));
    expect(checksums.get("oni-1.2.3-win32-x64.zip")).toBe("b".repeat(64));
    expect(checksums.size).toBe(2);
  });

  it("extracts with the System32 bsdtar on Windows and plain tar elsewhere", () => {
    expect(cliArchiveTarCommand("linux", {})).toBe("tar");
    expect(cliArchiveTarCommand("win32", { SystemRoot: "D:\\Win" })).toBe(
      "D:\\Win\\System32\\tar.exe",
    );
    expect(cliArchiveTarCommand("win32", {})).toBe("C:\\Windows\\System32\\tar.exe");
  });

  it("picks the newest non-draft release, skipping prerelease tags", () => {
    const releases = [
      { tag_name: "v1.2.5", draft: true },
      { tag_name: "v1.2.4-beta.1" },
      { tag_name: "desktop-preview" },
      { tag_name: "v1.2.3" },
      { tag_name: "v1.2.2" },
    ];
    expect(newestCliReleaseVersion(releases)).toBe("1.2.3");
    expect(newestCliReleaseVersion([{ tag_name: "v1.2.4-beta.1" }])).toBeUndefined();
  });
});
