import { useGT } from "gt-react";
import type { ProviderDriverKind } from "@onicode/contracts";
import { StarIcon } from "lucide-react";
import type { ReactNode, Ref } from "react";

import { cn } from "~/lib/utils";
import { DitherField } from "../DitherField";
import { Button } from "../ui/button";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";

export type ModelDetail = {
  name: string;
  driverKind: ProviderDriverKind;
  providerDisplayName: string;
  providerAccentColor?: string | undefined;
  acpRegistryAgentId?: string | undefined;
  acpRegistryIconUrl?: string | undefined;
  contextLabel: string | null;
  runsOn: string;
  isFavorite: boolean;
};

function Fact(props: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-muted-foreground">{props.label}</dt>
      <dd className="min-w-0 truncate font-medium tabular-nums">{props.value}</dd>
    </div>
  );
}

/**
 * What the picker knows about one model: its facts, the reasoning and options when it is the
 * selected model, and its place in Favorites. It floats beside the list, or sits under it when
 * the window has no room.
 */
export function ModelDetailCard({
  ref,
  ...props
}: {
  ref?: Ref<HTMLDivElement>;
  model: ModelDetail;
  /** The selected model's effort section; absent for other models. */
  controls?: ReactNode;
  floating: boolean;
  onToggleFavorite: () => void;
}) {
  const { model } = props;
  const gt = useGT();
  const favoriteLabel = model.isFavorite ? gt("Remove from favorites") : gt("Add to favorites");
  const footer = (
    <div className="p-1">
      <Button variant="secondary" size="sm" className="w-full" onClick={props.onToggleFavorite}>
        <StarIcon aria-hidden="true" className={cn(model.isFavorite && "fill-current")} />
        {favoriteLabel}
      </Button>
    </div>
  );

  if (!props.floating) {
    return (
      <div data-model-picker-card className="flex shrink-0 flex-col border-t border-border">
        {props.controls ? (
          <div className="flex max-h-48 min-h-0 flex-col">{props.controls}</div>
        ) : null}
        {footer}
      </div>
    );
  }

  return (
    <div
      ref={ref}
      data-model-picker-card
      role="group"
      aria-label={model.name}
      className="dropdown-glass absolute top-0 w-72 rounded-lg shadow-popup transition-opacity dark:shadow-popup-dark duration-150 starting:opacity-0 motion-reduce:transition-none"
    >
      <div className="relative flex flex-col overflow-hidden rounded-[inherit] bg-popover text-popover-foreground">
        <div className="relative overflow-hidden">
          <DitherField
            from={0}
            to={0.55}
            start={0.5}
            slant={-0.45}
            className="inset-x-0 top-0 h-16 w-full text-primary opacity-40 [mask-image:linear-gradient(to_bottom,black,transparent)]"
          />
          <div className="relative flex h-7 items-center gap-2 px-3 pt-1 font-medium text-sm">
            <ProviderInstanceIcon
              driverKind={model.driverKind}
              displayName={model.providerDisplayName}
              accentColor={model.providerAccentColor}
              acpRegistryAgentId={model.acpRegistryAgentId}
              acpRegistryIconUrl={model.acpRegistryIconUrl}
              className="size-3.5"
              iconClassName="size-3.5"
            />
            <span className="min-w-0 truncate">{model.name}</span>
          </div>
          <dl className="relative flex flex-col gap-1 px-3 pt-1 pb-2 text-xs">
            {model.contextLabel ? (
              <Fact
                label={gt("Context", { $context: "a model's context window size" })}
                value={model.contextLabel}
              />
            ) : null}
            <Fact
              label={gt("Runs on", { $context: "the provider a model runs on" })}
              value={model.runsOn}
            />
          </dl>
        </div>
        {props.controls ? <div className="border-t border-border">{props.controls}</div> : null}
        <div className="border-t border-border">{footer}</div>
      </div>
    </div>
  );
}
