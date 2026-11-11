import { AuthAccessReadScope, type EnvironmentId, type TailnetDevice } from "@onicode/contracts";
import { useState } from "react";

import { useEnvironments, usePrimaryEnvironmentId } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import { useEnvironmentsWithScope } from "~/state/session";
import { tailnetEnvironment } from "~/state/tailnet";
import { ConnectionStatusDot } from "../ConnectionStatusDot";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import {
  Command,
  CommandGroup,
  CommandGroupLabel,
  CommandInput,
  CommandItem,
  CommandList,
} from "../ui/command";
import { RefreshIcon } from "../ui/refresh-icon";
import { Spinner } from "../ui/spinner";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { groupTailnetDevices } from "@onicode/client-runtime/connection";

function TailnetDeviceRow({
  device,
  selfLabel,
  added,
  pending,
  disabled,
  onPick,
}: {
  readonly device: TailnetDevice;
  readonly selfLabel: string;
  readonly added: boolean;
  readonly pending: boolean;
  readonly disabled: boolean;
  readonly onPick: (device: TailnetDevice) => void;
}) {
  const onicodeLabel =
    device.onicode && device.onicode.label.toLocaleLowerCase() !== device.name.toLocaleLowerCase()
      ? `Onicode · ${device.onicode.label}`
      : "Onicode";
  return (
    <CommandItem value={device.id} disabled={disabled || added} onClick={() => onPick(device)}>
      <ConnectionStatusDot dotClassName={device.online ? "bg-success" : "bg-muted-foreground/40"} />
      <span
        className={
          device.online
            ? "flex min-w-0 flex-1 flex-col"
            : "flex min-w-0 flex-1 flex-col text-muted-foreground"
        }
      >
        <span className="truncate">{device.name}</span>
        <span className="truncate text-xs text-muted-foreground">
          {[device.addresses[0] ?? device.dnsName, device.online ? null : "Offline"]
            .filter(Boolean)
            .join(" · ")}
        </span>
      </span>
      {device.self ? (
        <Badge variant="outline" size="sm">
          {selfLabel}
        </Badge>
      ) : null}
      {device.onicode ? (
        <Badge variant="info" size="sm">
          {onicodeLabel}
        </Badge>
      ) : null}
      {added ? (
        <Badge variant="secondary" size="sm">
          Added
        </Badge>
      ) : null}
      {pending ? <Spinner size="sm" /> : null}
    </CommandItem>
  );
}

/**
 * Lists the tailnet as one connected server sees it. The local server lists it when it can,
 * since its machine is the one this client runs on.
 */
export function TailnetDevicePicker({
  addedEnvironmentIds,
  pendingDeviceId,
  disabled,
  onPick,
}: {
  readonly addedEnvironmentIds: ReadonlySet<EnvironmentId>;
  readonly pendingDeviceId: string | null;
  readonly disabled: boolean;
  readonly onPick: (device: TailnetDevice) => void;
}) {
  const { environments } = useEnvironments();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const supported = environments.filter(
    (environment) =>
      environment.connection.phase === "connected" &&
      environment.serverConfig?.environment.capabilities.tailnetDevices === true,
  );
  const readable = useEnvironmentsWithScope(supported, AuthAccessReadScope);
  const sources = supported.filter((environment) => readable.has(environment.environmentId));
  const source =
    sources.find((environment) => environment.environmentId === primaryEnvironmentId) ??
    sources[0] ??
    null;
  const devices = useEnvironmentQuery(
    source ? tailnetEnvironment.devices({ environmentId: source.environmentId, input: {} }) : null,
  );
  const [query, setQuery] = useState("");

  if (source === null) {
    return (
      <p className="rounded-lg border border-border/60 px-4 py-6 text-center text-sm text-muted-foreground">
        Connect an Onicode server running on a machine with Tailscale to list the devices on your
        tailnet.
      </p>
    );
  }

  const result = devices.data;
  const selfLabel =
    source.environmentId === primaryEnvironmentId ? "This device" : `Runs ${source.label}`;
  const groups = result?.available ? groupTailnetDevices(result.devices, query) : [];
  const renderBody = () => {
    if (result === null) {
      return devices.error ? (
        <p className="px-3 py-6 text-center text-sm text-destructive">{devices.error}</p>
      ) : (
        <p className="px-3 py-6 text-center text-sm text-muted-foreground">Loading devices…</p>
      );
    }
    if (!result.available) {
      return (
        <p className="px-3 py-6 text-center text-sm text-muted-foreground">
          Tailscale isn't running on {source.label}.
        </p>
      );
    }
    if (groups.length === 0) {
      return (
        <p className="break-all px-3 py-6 text-center text-sm text-muted-foreground">
          {query.trim().length > 0
            ? `No devices match "${query.trim()}".`
            : "No devices on this tailnet."}
        </p>
      );
    }
    return (
      <CommandList className="max-h-80 overflow-y-auto">
        {groups.map((group) => (
          <CommandGroup key={group.key}>
            <CommandGroupLabel>{group.title}</CommandGroupLabel>
            {group.devices.map((device) => (
              <TailnetDeviceRow
                key={device.id}
                device={device}
                selfLabel={selfLabel}
                added={
                  device.onicode !== undefined &&
                  addedEnvironmentIds.has(device.onicode.environmentId)
                }
                pending={pendingDeviceId === device.id}
                disabled={disabled}
                onPick={onPick}
              />
            ))}
          </CommandGroup>
        ))}
      </CommandList>
    );
  };

  return (
    <div className="rounded-lg border border-border/60">
      <div className="flex min-h-9 items-center gap-2 border-b border-border/60 px-3">
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {result?.tailnetName ? `${result.tailnetName} · ` : ""}
          Listed by {source.label}
        </span>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="icon-xs"
                variant="ghost-muted"
                aria-label="Refresh devices"
                disabled={devices.isPending}
                onClick={devices.refresh}
              >
                <RefreshIcon refreshing={devices.isPending} />
              </Button>
            }
          />
          <TooltipPopup side="top">Refresh devices</TooltipPopup>
        </Tooltip>
      </div>
      <Command
        mode="none"
        value={query}
        onValueChange={(value, eventDetails) => {
          if (eventDetails.reason !== "item-press") setQuery(value);
        }}
        aria-label="Choose a tailnet device"
      >
        <CommandInput
          placeholder="Search devices, people, tags, or addresses"
          disabled={disabled || result?.available !== true}
        />
        {renderBody()}
      </Command>
    </div>
  );
}
