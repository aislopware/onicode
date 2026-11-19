import type * as React from "react";

import { cn } from "~/lib/utils";

function Kbd({
  className,
  variant = "default",
  ...props
}: React.ComponentProps<"kbd"> & {
  /** `outline` draws a physical key, so the hint stays distinct inside surrounding text. */
  variant?: "default" | "outline";
}) {
  return (
    <kbd
      className={cn(
        "pointer-events-none inline-flex h-4.5 min-w-4.5 select-none items-center justify-center gap-0.5 rounded-sm px-1 font-normal font-sans text-muted-foreground text-xs [&_svg:not([class*='size-'])]:size-3",
        variant === "outline" ? "border border-b-2 border-foreground/15 px-1.5" : "bg-secondary",
        className,
      )}
      data-slot="kbd"
      {...props}
    />
  );
}

function KbdGroup({ className, ...props }: React.ComponentProps<"kbd">) {
  return (
    <kbd
      className={cn("inline-flex items-center gap-1", className)}
      data-slot="kbd-group"
      {...props}
    />
  );
}

export { Kbd, KbdGroup };
