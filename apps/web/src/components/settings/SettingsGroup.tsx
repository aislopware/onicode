import type { ComponentProps } from "react";

import { cn } from "../../lib/utils";

export function SettingsGroup({
  variant = "grouped",
  divided = true,
  className,
  ...props
}: ComponentProps<"div"> & {
  variant?: "grouped" | "plain";
  divided?: boolean;
}) {
  return (
    <div
      data-slot="settings-group"
      {...props}
      className={cn(
        "relative overflow-visible text-foreground",
        variant === "grouped"
          ? "rounded-lg border border-(--card-hairline) bg-card shadow-card"
          : "space-y-1",
        variant === "grouped" &&
          divided &&
          "*:not-first:border-t *:not-first:border-border [&>[data-slot=settings-row]]:rounded-none",
        className,
      )}
    />
  );
}
