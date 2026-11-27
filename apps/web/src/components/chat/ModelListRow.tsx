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
  "group/model-row flex h-7 w-full min-w-0 cursor-pointer items-center gap-2 rounded-md px-2 text-left text-ui text-foreground outline-none select-none data-highlighted:bg-row-highlight data-disabled:cursor-not-allowed data-disabled:opacity-55";

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
  /** The level a pick of this model starts at, said quietly after its name. */
  level?: string | null;
  disabledReason?: string | null;
  onToggleFavorite: (instanceId: ProviderInstanceId, slug: string) => void;
}) {
  const gt = useGT();
  const name = getDisplayModelName({
    slug: props.slug,
    name: props.name,
    subProvider: props.subProvider,
  });
  // Every row ends in one slot, so the star sits in the same place on each. At rest the slot
  // holds the shortcut, the check, or the star where it tells favorites apart; the pointer
  // brings the star in over any of them.
  const hasStar = !props.disabledReason;
  const checkInSlot = props.isSelected && !props.jumpLabel;
  const starRests =
    props.isFavorite && !props.quietFavorite && !props.jumpLabel && !props.isSelected;
  const yieldsToStar =
    hasStar &&
    "group-hover/model-row:invisible group-has-[button:focus-visible]/model-row:invisible";
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
      {props.level ? (
        // Gives way before the name does: the name is what the row is for.
        <span className="min-w-0 shrink-[99] truncate text-xs text-muted-foreground">
          {props.level}
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
        {/* Beside a shortcut, and where touch keeps the star in view, the check has its own place. */}
        {props.isSelected && (!checkInSlot || hasStar) ? (
          <CheckIcon
            aria-hidden="true"
            className={cn("size-3.5 text-foreground", checkInSlot && "hidden pointer-coarse:block")}
          />
        ) : null}
        <span className="relative flex h-5 min-w-5 items-center justify-end">
          {checkInSlot ? (
            <CheckIcon
              aria-hidden="true"
              className={cn(
                "size-3.5 text-foreground",
                hasStar && "pointer-coarse:hidden",
                yieldsToStar,
              )}
            />
          ) : null}
          {props.jumpLabel ? (
            <span data-model-jump-label className={cn(yieldsToStar, "pointer-coarse:invisible")}>
              {props.jumpLabel}
            </span>
          ) : null}
          {!hasStar ? null : (
            <button
              type="button"
              aria-label={props.isFavorite ? gt("Remove from favorites") : gt("Add to favorites")}
              aria-pressed={props.isFavorite}
              className={cn(
                // Hangs into the row's padding, so its glyph ends where the check and the shortcut do.
                "absolute inset-y-0 -right-[0.1875rem] flex w-5 cursor-pointer items-center justify-center rounded-sm outline-none hover:text-foreground focus-visible:outline-ring-inset",
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

  // Base UI's tooltip: the shared trigger stays shut on a disabled row, and this one explains why.
  return (
    <Tooltip>
      <TooltipTrigger render={row} />
      <TooltipPopup side="left" align="center">
        {props.disabledReason}
      </TooltipPopup>
    </Tooltip>
  );
});
