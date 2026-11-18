import { T, useGT } from "gt-react";
import { Combobox as ComboboxPrimitive } from "@base-ui/react/combobox";
import { type ProviderDriverKind, type ProviderInstanceId } from "@onicode/contracts";
import { CheckIcon, StarIcon } from "lucide-react";
import { memo, type ComponentProps } from "react";
import { cn } from "~/lib/utils";
import { Badge } from "../ui/badge";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";
import { getDisplayModelName } from "./providerIconUtils";

/** One look for every row the list highlights: model rows and the quiet "Show more" toggle. */
const MODEL_PICKER_ROW =
  "group/model-row flex h-7 w-full min-w-0 cursor-pointer items-center gap-2 rounded-md px-2 text-left text-sm text-foreground outline-none select-none data-highlighted:bg-row-highlight data-disabled:cursor-not-allowed data-disabled:opacity-50";

/** Passes every other prop on, so a tooltip can use the row as its trigger. */
export function ModelPickerRowItem({
  value,
  className,
  ...props
}: Omit<ComponentProps<typeof ComboboxPrimitive.Item>, "value" | "className"> & {
  value: string;
  className?: string;
}) {
  return (
    <ComboboxPrimitive.Item
      {...props}
      value={value}
      data-model-picker-row={value}
      className={cn(MODEL_PICKER_ROW, className)}
    />
  );
}

export const ModelListRow = memo(function ModelListRow(props: {
  index: number;
  rowKey: string;
  instanceId: ProviderInstanceId;
  slug: string;
  name: string;
  subProvider?: string | undefined;
  driverKind: ProviderDriverKind;
  providerDisplayName: string;
  providerAccentColor?: string | undefined;
  acpRegistryAgentId?: string | undefined;
  acpRegistryIconUrl?: string | undefined;
  isSelected: boolean;
  isFavorite: boolean;
  /** In the Favorites section every row is one, so its star waits for the pointer. */
  quietFavorite: boolean;
  /** Marks this instance apart from another of the same provider. */
  showInstanceBadge: boolean;
  showNewBadge?: boolean;
  unavailable?: boolean;
  jumpLabel?: string | null;
  disabledReason?: string | null;
  onToggleFavorite: (instanceId: ProviderInstanceId, slug: string) => void;
}) {
  const gt = useGT();
  const name = getDisplayModelName({
    slug: props.slug,
    name: props.name,
    subProvider: props.subProvider,
  });
  // The star rests in view only where it tells favorites apart; elsewhere it shows for the pointer.
  const starRests = props.isFavorite && !props.quietFavorite && !props.jumpLabel;
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
          const label = event.currentTarget;
          label.title = label.scrollWidth > label.clientWidth ? name : "";
        }}
      >
        {name}
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
        {props.isSelected ? (
          <CheckIcon aria-hidden="true" className="size-3.5 text-foreground" />
        ) : null}
        {/* The shortcut and the star share one slot, so rows end on the same edge. */}
        <span className="relative flex h-5 min-w-5 items-center justify-end">
          {props.jumpLabel ? (
            <span
              data-model-jump-label
              className={cn(
                !props.disabledReason && "group-hover/model-row:invisible pointer-coarse:invisible",
              )}
            >
              {props.jumpLabel}
            </span>
          ) : null}
          {props.disabledReason ? null : (
            <button
              type="button"
              aria-label={props.isFavorite ? gt("Remove from favorites") : gt("Add to favorites")}
              aria-pressed={props.isFavorite}
              className={cn(
                "absolute inset-y-0 right-0 flex w-5 cursor-pointer items-center justify-center rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
                !starRests &&
                  "opacity-0 group-hover/model-row:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100",
              )}
              // The row is an option: a press here must not pick the model.
              onClick={(event) => {
                event.stopPropagation();
                props.onToggleFavorite(props.instanceId, props.slug);
              }}
              onKeyDown={(event) => event.stopPropagation()}
            >
              <StarIcon
                aria-hidden="true"
                className={cn("size-3.5", props.isFavorite && "fill-current")}
              />
            </button>
          )}
        </span>
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
