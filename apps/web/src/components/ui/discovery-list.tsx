import type { ComponentProps, ReactNode } from "react";

export function DiscoveryList({ children }: { readonly children: ReactNode }) {
  return (
    <div className="flex flex-col divide-y divide-border overflow-hidden rounded-xl border border-border bg-background">
      {children}
    </div>
  );
}

export function DiscoveryListRow({
  icon,
  title,
  description,
  action,
  trailing,
  ...props
}: Omit<ComponentProps<"button">, "title" | "children" | "className"> & {
  readonly icon: ReactNode;
  readonly title: ReactNode;
  readonly description: ReactNode;
  readonly action?: ReactNode;
  /** A control beside the row, outside its button. */
  readonly trailing?: ReactNode;
}) {
  const row = (
    <button
      type="button"
      {...props}
      className="group flex w-full min-w-0 items-center gap-3 px-3 py-3 text-left hover:bg-accent/60 focus-visible:outline-ring-inset disabled:pointer-events-none disabled:opacity-55"
    >
      {icon}
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-sm font-medium text-foreground">{title}</span>
        <span className="truncate text-xs text-muted-foreground">{description}</span>
      </div>
      {action}
    </button>
  );
  if (trailing === undefined || trailing === null) return row;
  return (
    <div className="flex items-center gap-2 pr-3">
      {row}
      {trailing}
    </div>
  );
}
