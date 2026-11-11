import type { TailnetDevice } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { groupTailnetDevices } from "./tailnetDevices.ts";

const alice = { loginName: "alice@example.com", displayName: "Zoe Alice" };
const bob = { loginName: "bob@example.com", displayName: "Bob" };

function device(input: Partial<TailnetDevice> & Pick<TailnetDevice, "name">): TailnetDevice {
  return {
    id: input.name,
    addresses: ["100.64.0.1"],
    online: true,
    self: false,
    owner: alice,
    tags: [],
    ...input,
  };
}

const DEVICES = [
  device({ name: "ci-runner", owner: null, tags: ["tag:ci"] }),
  device({ name: "bob-laptop", owner: bob, addresses: ["100.64.0.9"] }),
  device({ name: "alice-old", online: false }),
  device({ name: "alice-studio", self: true, os: "macOS" }),
  device({ name: "alice-mini", addresses: ["100.101.7.3", "fd7a::1"] }),
];

describe("groupTailnetDevices", () => {
  it("puts the listing machine's owner first, tagged devices last, and offline devices last", () => {
    expect(
      groupTailnetDevices(DEVICES, "").map((group) => [
        group.title,
        group.devices.map((entry) => entry.name),
      ]),
    ).toEqual([
      ["Zoe Alice", ["alice-mini", "alice-studio", "alice-old"]],
      ["Bob", ["bob-laptop"]],
      ["Tagged devices", ["ci-runner"]],
    ]);
  });

  it.each([
    ["an address prefix", "100.101", ["alice-mini"]],
    ["the owner and OS together", "alice MACOS", ["alice-studio"]],
    ["a tag", "tag:ci", ["ci-runner"]],
    ["nothing", "windows", []],
  ])("matches %s", (_, query, names) => {
    expect(
      groupTailnetDevices(DEVICES, query).flatMap((group) =>
        group.devices.map((entry) => entry.name),
      ),
    ).toEqual(names);
  });
});
