import { T, useGT } from "gt-react";
import { Combobox as ComboboxPrimitive } from "@base-ui/react/combobox";
import { type ProviderDriverKind } from "@onicode/contracts";
import { CheckIcon } from "lucide-react";
import { memo, type ReactNode } from "react";
import { cn } from "~/lib/utils";
import { Badge } from "../ui/badge";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";

/** One look for every row the list highlights: model rows and the quiet "Show more" toggle. */
const MODEL_PICKER_ROW =
  "flex h-7 w-full min-w-0 cursor-pointer items-center gap-2 rounded-md px-2 text-left text-sm text-foreground outline-none select-none data-highlighted:bg-row-highlight data-disabled:cursor-not-allowed data-disabled:opacity-50";

export function ModelPickerRowItem(props: {
  index: number;
  value: string;
  disabled?: boolean;
  className?: string;
  "aria-expanded"?: boolean;
  children: ReactNode;
}) {
  return (
    <ComboboxPrimitive.Item
      index={props.index}
      value={props.value}
      disabled={props.disabled}
      aria-expanded={props["aria-expanded"]}
      data-model-picker-row={props.value}
      className={cn(MODEL_PICKER_ROW, props.className)}
    >
      {props.children}
    </ComboboxPrimitive.Item>
  );
}

export const ModelListRow = memo(function ModelListRow(props: {
  index: number;
  rowKey: string;
  name: string;
  subProvider?: string | undefined;
  driverKind: ProviderDriverKind;
  providerDisplayName: string;
  providerAccentColor?: string | undefined;
  acpRegistryAgentId?: string | undefined;
  acpRegistryIconUrl?: string | undefined;
  isSelected: boolean;
  /** Marks this instance apart from another of the same provider. */
  showInstanceBadge: boolean;
  showNewBadge?: boolean;
  unavailable?: boolean;
  contextLabel?: string | null;
  jumpLabel?: string | null;
  disabledReason?: string | null;
}) {
  const gt = useGT();
  const row = (
    <ModelPickerRowItem
      index={props.index}
      value={props.rowKey}
      disabled={Boolean(props.disabledReason)}
      // A disabled row still answers hover, so its reason can show.
      className={cn(props.disabledReason && "data-disabled:pointer-events-auto")}
    >
      <ProviderInstanceIcon
        driverKind={props.driverKind}
        displayName={props.providerDisplayName}
        accentColor={props.providerAccentColor}
        acpRegistryAgentId={props.acpRegistryAgentId}
        acpRegistryIconUrl={props.acpRegistryIconUrl}
        showBadge={props.showInstanceBadge}
        className="size-3.5"
        iconClassName="size-3.5"
        indicatorBackground="var(--popover)"
        badgeClassName="right-[-0.125rem] bottom-[-0.125rem] h-3 min-w-3 px-0.5 text-5xs"
      />
      <span
        className="min-w-0 truncate"
        // Names the row in full on hover only when it is cut off.
        onPointerEnter={(event) => {
          const name = event.currentTarget;
          name.title = name.scrollWidth > name.clientWidth ? props.name : "";
        }}
      >
        {props.name}
        {/* The logo is decorative, so name the provider for screen readers. */}
        <span className="sr-only">, {props.providerDisplayName}</span>
      </span>
      {props.subProvider ? (
        <span className="min-w-0 shrink truncate text-xs text-muted-foreground">
          {props.subProvider}
        </span>
      ) : null}
      {props.showNewBadge ? (
        <span
          className="shrink-0 rounded-sm border border-update/35 bg-update/15 px-0.5 py-px text-3xs font-bold uppercase leading-none tracking-wide text-update-foreground"
          aria-label={gt("New model")}
        >
          {gt("New", { $context: "badge on a newly added model" })}
        </span>
      ) : null}
      {props.unavailable ? (
        <Badge variant="outline" size="sm">
          <T context="badge on a model that cannot be selected">Unavailable</T>
        </Badge>
      ) : null}
      <span className="ms-auto flex shrink-0 items-center gap-2 ps-2 text-xs text-muted-foreground tabular-nums">
        {props.contextLabel ? <span>{props.contextLabel}</span> : null}
        {props.isSelected ? (
          <CheckIcon aria-hidden="true" className="size-3.5 text-foreground" />
        ) : null}
        {props.jumpLabel ? (
          <span data-model-jump-label className="text-muted-foreground">
            {props.jumpLabel}
          </span>
        ) : null}
      </span>
    </ModelPickerRowItem>
  );

  if (!props.disabledReason) {
    return row;
  }

  return (
    <Tooltip>
      <TooltipTrigger render={row} />
      <TooltipPopup side="left" align="center">
        {props.disabledReason}
      </TooltipPopup>
    </Tooltip>
  );
});
