import type { TailnetDevice } from "@onicode/contracts";
import { foldSearchQuery } from "@onicode/shared/paletteSearch";

export interface TailnetDeviceGroup {
  readonly key: string;
  /** The owner's display name, or "Tagged devices". */
  readonly title: string;
  readonly devices: ReadonlyArray<TailnetDevice>;
}

const TAGGED_GROUP_KEY = "tagged";

function deviceSearchText(device: TailnetDevice): string {
  return foldSearchQuery(
    [
      device.name,
      device.dnsName,
      ...device.addresses,
      device.os,
      device.owner?.displayName,
      device.owner?.loginName,
      ...device.tags,
      device.onicode?.label,
    ]
      .filter((value) => value !== undefined)
      .join(" "),
  );
}

/**
 * Groups devices the way Tailscale's Devices list does: by owner, with the listing machine's
 * owner first and tagged devices last. Online devices come before offline ones in each group.
 * Every word of `query` must appear in a device's name, address, OS, owner, tags, or label.
 */
export function groupTailnetDevices(
  devices: ReadonlyArray<TailnetDevice>,
  query: string,
): TailnetDeviceGroup[] {
  const tokens = foldSearchQuery(query).split(" ").filter(Boolean);
  const matches =
    tokens.length === 0
      ? devices
      : devices.filter((device) => {
          const text = deviceSearchText(device);
          return tokens.every((token) => text.includes(token));
        });
  const selfOwner = devices.find((device) => device.self)?.owner?.loginName;
  const groups = new Map<string, { title: string; devices: TailnetDevice[] }>();
  for (const device of matches) {
    const key = device.owner?.loginName ?? TAGGED_GROUP_KEY;
    const group = groups.get(key);
    if (group) group.devices.push(device);
    else
      groups.set(key, {
        title: device.owner?.displayName ?? "Tagged devices",
        devices: [device],
      });
  }
  const rank = (key: string) => (key === selfOwner ? 0 : key === TAGGED_GROUP_KEY ? 2 : 1);
  return [...groups]
    .sort(
      ([leftKey, left], [rightKey, right]) =>
        rank(leftKey) - rank(rightKey) || left.title.localeCompare(right.title),
    )
    .map(([key, group]) => ({
      key,
      title: group.title,
      devices: group.devices.sort(
        (left, right) =>
          Number(right.online) - Number(left.online) || left.name.localeCompare(right.name),
      ),
    }));
}
