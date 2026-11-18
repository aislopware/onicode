import { Toolbar } from "@base-ui/react/toolbar";
import { type ProviderInstanceId } from "@onicode/contracts";
import { memo, useState } from "react";
import { msg, T, useMessages } from "gt-react";
import { StarIcon } from "lucide-react";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { cn } from "~/lib/utils";
import type { MessageResolver } from "../../i18n/messages";
import {
  isProviderInstancePickerReady,
  shouldShowInstanceBadge,
  type ProviderInstanceEntry,
} from "../../providerInstances";

function describeUnavailableInstance(
  entry: ProviderInstanceEntry,
  resolve: MessageResolver,
): string {
  const label = entry.displayName;
  // An unavailable instance (a driver this build lacks or the policy leaves out) says why instead.
  if ((!entry.enabled && entry.isAvailable) || entry.status === "disabled") {
    return resolve(msg("{label}: Disabled in settings.", { label }));
  }
  if (entry.status === "ready" && entry.isAvailable) {
    return label;
  }
  const message = entry.snapshot.message?.trim();
  if (message) {
    return resolve(
      entry.status === "error"
        ? msg("{label}: Unavailable. {message}", { label, message })
        : entry.status === "warning"
          ? msg("{label}: Limited. {message}", { label, message })
          : msg("{label}: Not ready. {message}", { label, message }),
    );
  }
  return resolve(
    entry.status === "error"
      ? msg("{label}: Unavailable.", { label })
      : entry.status === "warning"
        ? msg("{label}: Limited.", { label })
        : msg("{label}: Not ready.", { label }),
  );
}

const PICKER_TOOLTIP_SIDE = "top" as const;
const PICKER_TOOLTIP_SIDE_OFFSET = 6;
// One look for every tab: the chosen one keeps the fill the others only get on hover.
const TAB_FILL = "color-mix(in srgb, var(--foreground) 9%, var(--popover))";
const TAB_BUTTON =
  "relative isolate flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-foreground/[0.09] hover:text-foreground focus-visible:bg-foreground/[0.09] focus-visible:text-foreground focus-visible:outline-none aria-pressed:bg-foreground/[0.09] aria-pressed:text-foreground";

export const ModelPickerSidebar = memo(function ModelPickerSidebar(props: {
  selectedInstanceId: ProviderInstanceId | "favorites";
  onSelectInstance: (instanceId: ProviderInstanceId | "favorites") => void;
  onFocusSearch: () => void;
  instanceEntries: ReadonlyArray<ProviderInstanceEntry>;
  showFavorites?: boolean;
  disabledInstanceIds?: ReadonlySet<ProviderInstanceId>;
  selectableUnavailableInstanceIds?: ReadonlySet<ProviderInstanceId>;
  getDisabledInstanceTooltip?: (entry: ProviderInstanceEntry) => string;
}) {
  const m = useMessages();
  const handleSelect = (instanceId: ProviderInstanceId | "favorites") => {
    props.onSelectInstance(instanceId);
  };
  const showFavorites = props.showFavorites ?? true;
  const [hoveredInstanceId, setHoveredInstanceId] = useState<ProviderInstanceId | null>(null);

  return (
    <Toolbar.Root
      className="shrink-0"
      data-model-picker-sidebar="true"
      aria-label={m(msg("Providers"))}
      orientation="horizontal"
      onKeyDown={(event) => {
        if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
        // The list is driven from the search field, so Up and Down both return to it.
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          props.onFocusSearch();
          return;
        }
      }}
    >
      <div className="flex items-center gap-0.5 overflow-x-auto overscroll-x-contain px-1.75 pt-1.5 [-ms-overflow-style:none] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        {showFavorites ? (
          <div className="relative" data-model-picker-provider="favorites">
            <Tooltip>
              <TooltipTrigger
                render={
                  <Toolbar.Button
                    className={TAB_BUTTON}
                    onClick={() => handleSelect("favorites")}
                    type="button"
                    aria-label={m(msg("Favorites"))}
                    aria-pressed={props.selectedInstanceId === "favorites"}
                  >
                    <StarIcon className="size-4.5 shrink-0" aria-hidden />
                  </Toolbar.Button>
                }
              />
              <TooltipPopup
                side={PICKER_TOOLTIP_SIDE}
                sideOffset={PICKER_TOOLTIP_SIDE_OFFSET}
                align="center"
              >
                <T>Favorites</T>
              </TooltipPopup>
            </Tooltip>
          </div>
        ) : null}

        {props.instanceEntries.map((entry) => {
          const isUnavailable = !isProviderInstancePickerReady(entry);
          const isContextDisabled = props.disabledInstanceIds?.has(entry.instanceId) ?? false;
          const unavailableSelectionIsReachable =
            props.selectableUnavailableInstanceIds?.has(entry.instanceId) ?? false;
          const isDisabled =
            (isUnavailable && !unavailableSelectionIsReachable) || isContextDisabled;
          const isSelected = props.selectedInstanceId === entry.instanceId;
          const isHovered = hoveredInstanceId === entry.instanceId;
          const showInstanceBadge = shouldShowInstanceBadge(entry, props.instanceEntries);

          const tooltip = isUnavailable
            ? describeUnavailableInstance(entry, m)
            : isContextDisabled
              ? (props.getDisabledInstanceTooltip?.(entry) ?? entry.displayName)
              : entry.displayName;

          const button = (
            <Toolbar.Button
              className={cn(
                TAB_BUTTON,
                isDisabled && "cursor-not-allowed opacity-50 hover:bg-transparent",
              )}
              data-provider-accent-color={entry.accentColor}
              onClick={() => !isDisabled && handleSelect(entry.instanceId)}
              onMouseEnter={() => setHoveredInstanceId(entry.instanceId)}
              onMouseLeave={() =>
                setHoveredInstanceId((current) => (current === entry.instanceId ? null : current))
              }
              onFocus={() => setHoveredInstanceId(entry.instanceId)}
              onBlur={() =>
                setHoveredInstanceId((current) => (current === entry.instanceId ? null : current))
              }
              disabled={isDisabled}
              focusableWhenDisabled={!isDisabled}
              aria-pressed={isSelected}
              type="button"
              aria-label={isUnavailable || isContextDisabled ? tooltip : entry.displayName}
            >
              <ProviderInstanceIcon
                driverKind={entry.driverKind}
                displayName={entry.displayName}
                accentColor={entry.accentColor}
                acpRegistryAgentId={entry.acpRegistryAgentId}
                acpRegistryIconUrl={entry.acpRegistryIconUrl}
                showBadge={showInstanceBadge}
                className="z-30 size-5"
                iconClassName="size-4.5"
                indicatorBackground={
                  (isHovered && !isDisabled) || isSelected ? TAB_FILL : "var(--popover)"
                }
                {...(entry.accentColor ? { badgeClassName: "h-3 min-w-3 px-0.5 text-5xs" } : {})}
              />
            </Toolbar.Button>
          );

          const trigger = isDisabled ? <span className="relative block">{button}</span> : button;

          return (
            <div
              key={entry.instanceId}
              className="relative"
              data-model-picker-provider={entry.instanceId}
            >
              <Tooltip>
                <TooltipTrigger render={trigger} />
                <TooltipPopup
                  side={PICKER_TOOLTIP_SIDE}
                  sideOffset={PICKER_TOOLTIP_SIDE_OFFSET}
                  align="center"
                >
                  {tooltip}
                </TooltipPopup>
              </Tooltip>
            </div>
          );
        })}
      </div>
    </Toolbar.Root>
  );
});
