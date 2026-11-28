import { Tabs } from "@base-ui/react/tabs";
import type { ProviderInstanceId } from "@onicode/contracts";
import { useGT } from "gt-react";
import { LayoutGridIcon } from "lucide-react";
import type { ReactNode } from "react";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";
import { SharedTooltipTrigger } from "../ui/shared-tooltip";
import type { ProviderInstanceEntry } from "../../providerInstances";

export type ModelPickerTab = "all" | ProviderInstanceId;

/** The model picker's provider column: All, then one tab per provider showing only its models. */
export function ModelPickerProviderTabs(props: {
  value: ModelPickerTab;
  onValueChange: (tab: ModelPickerTab) => void;
  entries: ReadonlyArray<ProviderInstanceEntry>;
  badgedInstanceIds: ReadonlySet<ProviderInstanceId>;
  isLockedOut: (entry: ProviderInstanceEntry) => boolean;
}) {
  const gt = useGT();
  const tab = (value: ModelPickerTab, label: string, icon: ReactNode, disabled = false) => (
    <SharedTooltipTrigger
      key={value}
      side="left"
      sideOffset={10}
      label={label}
      render={
        <Tabs.Tab
          value={value}
          aria-label={label}
          disabled={disabled}
          className="group/tab relative flex h-8 w-full shrink-0 cursor-pointer items-center justify-center rounded-(--control-radius) text-muted-foreground outline-none transition-colors duration-100 hover:text-foreground focus-visible:outline-ring-inset data-active:text-foreground data-disabled:cursor-not-allowed data-disabled:opacity-40"
        />
      }
    >
      {icon}
    </SharedTooltipTrigger>
  );

  return (
    <Tabs.Root
      orientation="vertical"
      value={props.value}
      onValueChange={(value: ModelPickerTab) => props.onValueChange(value)}
      className="flex w-11 shrink-0 flex-col border-e border-border bg-secondary dark:bg-white/4.5"
    >
      <Tabs.List
        aria-label={gt("Providers")}
        activateOnFocus
        className="relative isolate flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto overscroll-contain p-1.5 [scrollbar-width:none]"
      >
        {tab("all", gt("All models"), <LayoutGridIcon className="size-3.5" aria-hidden />)}
        <div aria-hidden className="mx-2 my-1 h-px shrink-0 bg-border" />
        {props.entries.map((entry) =>
          tab(
            entry.instanceId,
            entry.displayName,
            <ProviderInstanceIcon
              driverKind={entry.driverKind}
              displayName={entry.displayName}
              accentColor={entry.accentColor}
              acpRegistryAgentId={entry.acpRegistryAgentId}
              acpRegistryIconUrl={entry.acpRegistryIconUrl}
              showBadge={props.badgedInstanceIds.has(entry.instanceId)}
              // Brand colour marks the chosen provider; the rest stay quiet until pointed at.
              className={
                props.value === entry.instanceId
                  ? "size-4"
                  : "size-4 opacity-55 grayscale transition-[filter,opacity] duration-100 group-hover/tab:opacity-100 group-hover/tab:grayscale-0"
              }
              iconClassName="size-4"
              indicatorBackground="var(--popover)"
              badgeClassName="right-[-0.125rem] bottom-[-0.125rem] h-3 min-w-3 px-0.5 text-5xs"
            />,
            props.isLockedOut(entry),
          ),
        )}
        <Tabs.Indicator
          // A keyboard move lands at once: the plate must not trail the focus ring.
          className="pointer-events-none absolute top-0 left-0 -z-1 h-(--active-tab-height) w-(--active-tab-width) translate-x-(--active-tab-left) translate-y-(--active-tab-top) rounded-(--control-radius) bg-card shadow-raised transition-[translate,height] duration-200 ease-out-quint motion-reduce:transition-none in-[[role=tablist]:has(:focus-visible)]:transition-none dark:bg-white/8"
        />
      </Tabs.List>
    </Tabs.Root>
  );
}
