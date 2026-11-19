import { useGT } from "gt-react";
import { ArrowLeftIcon, FileDiffIcon } from "lucide-react";
import { useEffect, useRef, type ComponentProps, type ReactNode } from "react";

import { cn } from "~/lib/utils";

import { Tabs, TabsList, TabsTab } from "../ui/tabs";
import { PullRequestMetaLine } from "./pullRequestPresentation";

export function PullRequestDetailHeaderBody({
  title,
  author,
  updated,
  checkout,
  base,
  head,
  files,
  diffStat,
}: {
  title: ReactNode;
  author: ReactNode;
  updated: ReactNode;
  checkout?: ReactNode;
  base: ReactNode;
  head: ReactNode;
  files: ReactNode;
  diffStat: ReactNode;
}) {
  const gt = useGT();
  return (
    <div className="col-span-2 mt-1 min-w-0 px-4 pb-4">
      {title}
      <div className="mt-2 flex min-h-5 min-w-0 items-center gap-2 text-xs text-muted-foreground">
        <PullRequestMetaLine className="min-w-0 whitespace-nowrap">
          {author}
          {updated}
        </PullRequestMetaLine>
        {checkout}
      </div>
      <div className="mt-4 flex min-h-5 min-w-0 items-center gap-2 text-xs text-muted-foreground">
        <span className="flex min-w-0 flex-1 items-center gap-1.5 font-mono text-xs text-muted-foreground">
          {base}
          <ArrowLeftIcon
            aria-label={gt("receives changes from", {
              $context:
                "screen reader label for an arrow between the base branch, read before it, and the head branch, read after it",
            })}
            className="size-3.5 shrink-0 opacity-60"
          />
          {head}
        </span>
        <span className="ml-auto inline-flex shrink-0 items-baseline justify-end gap-2">
          <span className="inline-flex min-w-16 items-baseline justify-end gap-1.5 tabular-nums">
            <FileDiffIcon aria-hidden className="size-3.5 self-center" />
            {files}
          </span>
          {diffStat}
        </span>
      </div>
    </div>
  );
}

export function PullRequestDetailTitleRow({ className, ...props }: ComponentProps<"div">) {
  return (
    <div className={cn("flex min-h-7 min-w-0 items-center sm:min-h-6", className)} {...props} />
  );
}

export function pullRequestTabId(panels: string, tab: string): string {
  return `${panels}-tab-${tab}`;
}

export function pullRequestTabPanelId(panels: string, tab: string): string {
  return `${panels}-panel-${tab}`;
}

export function PullRequestDetailTabBar<Tab extends string>({
  tabs,
  value,
  onValueChange,
  onTabIntent,
  panels,
  inert,
  children,
}: {
  tabs: ReadonlyArray<{ readonly value: Tab; readonly label: string }>;
  value: Tab;
  onValueChange?: (tab: Tab) => void;
  onTabIntent?: (tab: Tab) => void;
  /** Id prefix shared with the tab panels, so each tab names the panel it shows. */
  panels?: string;
  inert?: boolean;
  children?: ReactNode;
}) {
  const gt = useGT();
  // A pointer passing over a tab is not intent; it must rest like a list row does.
  const pointerIntent = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- A rest timer still pending when the bar unmounts must not read for a closed panel.
  useEffect(() => () => clearTimeout(pointerIntent.current), []);
  return (
    <nav
      // Reversed wrap: actions that do not fit move above the tabs, and `items-start` is the bottom edge.
      className="col-span-2 flex min-w-0 flex-wrap-reverse items-start gap-x-4 border-t border-border px-4"
      aria-label={gt("Pull request tabs")}
      inert={inert}
    >
      <Tabs
        className="shrink-0"
        value={value}
        onValueChange={(next) => {
          const nextTab = tabs.find((item) => item.value === next)?.value;
          if (nextTab) onValueChange?.(nextTab);
        }}
      >
        <TabsList>
          {tabs.map((tab) => (
            <TabsTab
              key={tab.value}
              value={tab.value}
              id={panels ? pullRequestTabId(panels, tab.value) : undefined}
              aria-controls={panels ? pullRequestTabPanelId(panels, tab.value) : undefined}
              onPointerEnter={
                onTabIntent
                  ? () => {
                      clearTimeout(pointerIntent.current);
                      pointerIntent.current = setTimeout(() => onTabIntent(tab.value), 150);
                    }
                  : undefined
              }
              onPointerLeave={onTabIntent ? () => clearTimeout(pointerIntent.current) : undefined}
              onFocus={onTabIntent ? () => onTabIntent(tab.value) : undefined}
            >
              {tab.label}
            </TabsTab>
          ))}
        </TabsList>
      </Tabs>
      <div className="flex min-h-9 grow flex-wrap items-center gap-2 py-1.5">{children}</div>
    </nav>
  );
}

export function PullRequestChecksStatusLine({
  icon,
  label,
  className,
  ...props
}: Omit<ComponentProps<"span">, "children"> & { icon: ReactNode; label: ReactNode }) {
  return (
    <span
      className={cn(
        "ml-auto flex h-4 min-w-0 flex-1 flex-wrap content-start items-center justify-end gap-x-1.5 overflow-hidden text-xs",
        className,
      )}
      {...props}
    >
      {icon}
      <span className="whitespace-nowrap">{label}</span>
    </span>
  );
}
