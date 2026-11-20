import { EnvironmentId, type HostHardwareSnapshot } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { describeHostHardware, describeHostHardwareAlert, formatCapacity } from "./hostHardware.ts";

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
          detail: "14 cores (10 performance, 4 efficiency) · Load 4.2",
          value: "23% in use",
          fraction: 0.234,
          level: "normal",
          warning: null,
        },
        {
          kind: "memory",
          key: "memory",
          title: "48 GB memory",
          detail: "31.2 GB used",
          value: "16.8 GB available",
          fraction: expect.closeTo(0.65, 9),
          level: "normal",
          warning: null,
        },
        {
          kind: "storage",
          key: "storage:/",
          title: "926 GB storage",
          detail: "751 GB used",
          value: "175 GB available",
          fraction: expect.closeTo(1 - 175 / 926, 9),
          level: "normal",
          warning: null,
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
        detail: "494 GB used · /work · Onicode data, worktrees, projects",
        value: "6 GB available",
        level: "critical",
      },
    ]);
  });
});

describe("describeHostHardwareAlert", () => {
  const studio = EnvironmentId.make("studio");
  const runner = EnvironmentId.make("runner");
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

  it.each([
    // 94% of a 926 GB disk is used, yet 60 GB is plenty; a saturated processor is no warning.
    [withFree(16, 60), null],
    [{ ...withFree(16, 60), usage: { ...mac.usage, cpuUtilization: 1 } }, null],
    [
      withFree(16, 24),
      {
        level: "high",
        kind: "storage",
        title: "Low storage on Studio",
        description: "24 GB available of 926 GB storage",
      },
    ],
    [
      withFree(3.5, 60),
      {
        level: "high",
        kind: "memory",
        title: "Low memory on Studio",
        description: "3.5 GB available of 48 GB memory",
      },
    ],
    [
      withFree(1.5, 24),
      {
        level: "critical",
        kind: "memory",
        title: "Low memory and low storage on Studio",
        description: "1.5 GB available of 48 GB memory, 24 GB available of 926 GB storage",
      },
    ],
    // A minute-long burst over 14 CPUs is no warning; the 5-minute average has to agree.
    [withLoad(withFree(16, 60), 60, 18), null],
    [
      withLoad(withFree(16, 60), 46.28, 37.06),
      {
        level: "high",
        kind: "cpu",
        title: "Heavy processor load on Studio",
        description: "Load 37 on 14 CPUs",
      },
    ],
    [
      withLoad(withFree(3.5, 60), 45, 50),
      {
        level: "critical",
        kind: "cpu",
        title: "Low memory and heavy processor load on Studio",
        description: "Load 45 on 14 CPUs, 3.5 GB available of 48 GB memory",
      },
    ],
    [
      withLoad(withFree(1.5, 9), 46.28, 37.06),
      {
        level: "critical",
        kind: "memory",
        title: "Low memory, low storage and heavy processor load on Studio",
        description:
          "1.5 GB available of 48 GB memory, 9 GB available of 926 GB storage, Load 37 on 14 CPUs",
      },
    ],
  ])(
    "warns when memory or storage runs out or work queues for the processor (%#)",
    (hardware, expected) => {
      const alert = describeHostHardwareAlert([
        { environmentId: studio, label: "Studio", hardware },
      ]);
      expect(
        alert && {
          level: alert.level,
          kind: alert.kind,
          title: alert.title,
          description: alert.description,
        },
      ).toEqual(expected);
    },
  );

  it("leads to the machine in the worst state and keys on what is low", () => {
    const machines = [
      { environmentId: studio, label: "Studio", hardware: withFree(16, 24) },
      { environmentId: runner, label: "Runner", hardware: withFree(16, 9) },
    ];

    expect(describeHostHardwareAlert(machines)).toEqual({
      key: "runner:storage:/:critical|studio:storage:/:high",
      level: "critical",
      kind: "storage",
      title: "Low storage on 2 machines",
      description:
        "Runner: 9 GB available of 926 GB storage\nStudio: 24 GB available of 926 GB storage",
      environmentId: runner,
    });
    // Free space moving within the same level is the same warning.
    expect(
      describeHostHardwareAlert([machines[0]!, { ...machines[1]!, hardware: withFree(16, 7) }])
        ?.key,
    ).toBe("runner:storage:/:critical|studio:storage:/:high");
  });
});
