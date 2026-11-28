import type { ComponentProps, ReactNode } from "react";
import { SharedTooltipTrigger } from "./ui/shared-tooltip";
import { TooltipPopup } from "./ui/tooltip";
import { cn } from "~/lib/utils";

const HOVER_CARD_CLASS_NAME = "max-w-80 text-left whitespace-normal";

// Shift, not flip: a card that grows under the pointer (Show more, a live status) keeps covering
// it, so it does not close under a hand that never moved.
export const HOVER_CARD_COLLISION = { side: "flip", align: "shift" } as const;

export function ThreadHoverCardPopup({ className, ...props }: ComponentProps<typeof TooltipPopup>) {
  return (
    <TooltipPopup {...props} variant="glass" className={cn(HOVER_CARD_CLASS_NAME, className)} />
  );
}

/** A thread hover card on `render`, drawn by the shared tooltip so a list of rows mounts none. */
export function ThreadHoverCardTrigger({
  card,
  side = "right",
  align = "start",
  sideOffset = 4,
  collisionAvoidance = HOVER_CARD_COLLISION,
  ...props
}: Omit<ComponentProps<typeof SharedTooltipTrigger>, "label" | "variant" | "className"> & {
  card: ReactNode;
}) {
  return (
    <SharedTooltipTrigger
      {...props}
      label={card}
      side={side}
      align={align}
      sideOffset={sideOffset}
      collisionAvoidance={collisionAvoidance}
      variant="glass"
      className={HOVER_CARD_CLASS_NAME}
    />
  );
}

export function ThreadHoverCard({
  title,
  children,
  footer,
}: {
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <div className="flex min-w-0 max-w-80 flex-col gap-2 px-1 py-2">
      <div className="line-clamp-3 min-w-0 wrap-break-word text-xs leading-tight font-medium text-foreground">
        {title}
      </div>
      <div className="grid gap-1.5 pl-0.5 text-xs text-muted-foreground">{children}</div>
      {footer}
    </div>
  );
}
