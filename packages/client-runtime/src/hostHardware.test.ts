import type { HostHardwareSnapshot } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { describeHostHardware, formatCapacity } from "./hostHardware.ts";

const GiB = 1024 ** 3;

const mac: HostHardwareSnapshot = {
  hostname: "studio.local",
  model: "Mac mini (2024)",
  os: "macOS 15.2",
  cpu: {
    model: "Apple M4 Pro",
    logicalCores: 14,
    physicalCores: 14,
    coreGroups: [
      { name: "Performance", cores: 10 },
      { name: "Efficiency", cores: 4 },
    ],
  },
  memoryBytes: 48 * GiB,
  uptimeSeconds: 4 * 86_400 + 120,
  volumes: [
    { mountPoint: "/", totalBytes: 926 * GiB, availableBytes: 175 * GiB, uses: ["home", "data"] },
  ],
  usage: {
    sampledAt: 0,
    cpuUtilization: 0.234,
    cpuCount: 14,
    availableMemoryBytes: 16.8 * GiB,
    totalMemoryBytes: 48 * GiB,
    loadAverage: { oneMinute: 4.24, fiveMinutes: 6.3 },
  },
};

describe("formatCapacity", () => {
  it.each([
    [0, "0 B"],
    [512 * 1024 ** 2, "512 MB"],
    [7.5 * GiB, "7.5 GB"],
    [16.84 * GiB, "16.8 GB"],
    [48 * GiB, "48 GB"],
    [926.4 * GiB, "926 GB"],
    // Rounds to the next unit instead of reading "1024 GB".
    [1023.9 * GiB, "1 TB"],
    [1.82 * 1024 * GiB, "1.82 TB"],
  ])("formats %d bytes as %s", (bytes, text) => {
    expect(formatCapacity(bytes)).toBe(text);
  });
});

describe("describeHostHardware", () => {
  it("names the machine and gauges its processor, memory, and storage", () => {
    expect(describeHostHardware(mac)).toEqual({
      name: "Mac mini (2024)",
      facts: ["macOS 15.2", "studio.local", "Up 4 days"],
      gauges: [
        {
          kind: "cpu",
          key: "cpu",
          title: "Apple M4 Pro",
          detail: "14 cores (10 performance and 4 efficiency) · Load 4.2",
          value: "23% in use",
          fraction: 0.234,
          level: "normal",
        },
        {
          kind: "memory",
          key: "memory",
          title: "48 GB memory",
          detail: "31.2 GB used",
          value: "16.8 GB available",
          fraction: expect.closeTo(0.65, 9),
          level: "normal",
        },
        {
          kind: "storage",
          key: "storage:/",
          title: "926 GB storage",
          detail: "751 GB used",
          value: "175 GB available",
          fraction: expect.closeTo(1 - 175 / 926, 9),
          level: "normal",
        },
      ],
    });
  });

  it("describes a capped container on an unnamed machine with several volumes", () => {
    const view = describeHostHardware({
      ...mac,
      hostname: "ci-runner",
      model: null,
      os: "Ubuntu 24.04.1 LTS",
      cpu: { model: null, logicalCores: 16, physicalCores: 8, coreGroups: [] },
      memoryBytes: 64 * GiB,
      uptimeSeconds: 2 * 3_600,
      volumes: [
        { mountPoint: "/", totalBytes: 100 * GiB, availableBytes: 8 * GiB, uses: ["home"] },
        {
          mountPoint: "/work",
          totalBytes: 500 * GiB,
          availableBytes: 6 * GiB,
          uses: ["data", "worktrees", "projects"],
        },
      ],
      usage: {
        sampledAt: 0,
        cpuUtilization: null,
        cpuCount: 4,
        availableMemoryBytes: 0.5 * GiB,
        totalMemoryBytes: 8 * GiB,
      },
    });

    // The hostname stands in for the missing model, so it is not repeated as a fact.
    expect(view.name).toBe("ci-runner");
    expect(view.facts).toEqual(["Ubuntu 24.04.1 LTS", "Up 2 hours"]);
    expect(
      view.gauges.map(({ title, detail, value, level }) => ({ title, detail, value, level })),
    ).toEqual([
      {
        title: "Processor",
        detail: "8 cores, 16 threads, 4 usable",
        value: null,
        level: "normal",
      },
      {
        title: "64 GB memory",
        detail: "7.5 GB used of 8 GB usable",
        value: "512 MB available",
        level: "high",
      },
      {
        title: "100 GB storage",
        detail: "92 GB used · / · Home",
        value: "8 GB available",
        level: "high",
      },
      {
        title: "500 GB storage",
        detail: "494 GB used · /work · Onicode data, worktrees, and projects",
        value: "6 GB available",
        level: "critical",
      },
    ]);
  });

  const withFree = (memoryGiB: number, storageGiB: number): HostHardwareSnapshot => ({
    ...mac,
    volumes: [{ ...mac.volumes[0]!, availableBytes: storageGiB * GiB }],
    usage: { ...mac.usage, availableMemoryBytes: memoryGiB * GiB },
  });
  const withLoad = (
    hardware: HostHardwareSnapshot,
    oneMinute: number,
    fiveMinutes: number,
  ): HostHardwareSnapshot => ({
    ...hardware,
    usage: { ...hardware.usage, loadAverage: { oneMinute, fiveMinutes } },
  });

  // Levels of the processor, memory, and storage gauges, in that order.
  it.each([
    // 94% of a 926 GB disk is used, yet 60 GB is plenty; a saturated processor is not strained.
    [
      { ...withFree(16, 60), usage: { ...mac.usage, cpuUtilization: 1 } },
      ["normal", "normal", "normal"],
    ],
    [withFree(16, 24), ["normal", "normal", "high"]],
    [withFree(3.5, 60), ["normal", "high", "normal"]],
    [withFree(1.5, 9), ["normal", "critical", "critical"]],
    // A minute-long burst over 14 CPUs is not strained; the 5-minute average has to agree.
    [withLoad(withFree(16, 60), 60, 18), ["normal", "normal", "normal"]],
    [withLoad(withFree(16, 60), 46.28, 37.06), ["high", "normal", "normal"]],
    [withLoad(withFree(16, 60), 45, 50), ["critical", "normal", "normal"]],
  ])("levels each gauge by how close it is to running out (%#)", (hardware, levels) => {
    expect(describeHostHardware(hardware).gauges.map((gauge) => gauge.level)).toEqual(levels);
  });
});
