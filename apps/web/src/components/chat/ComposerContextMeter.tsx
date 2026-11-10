import type { ContextWindowSnapshot } from "@onicode/client-runtime/state/context-window";
import { memo } from "react";

import { formatContextWindowTokens } from "../../lib/contextWindow";
import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

const RADIUS = 6;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** How full the context window is, as a ring beside send; it opens the breakdown when the provider has one. */
export const ComposerContextMeter = memo(function ComposerContextMeter(props: {
  readonly usage: ContextWindowSnapshot;
  readonly onOpen?: (() => void) | undefined;
}) {
  const { usage } = props;
  const maxTokens = usage.maxTokens != null && usage.maxTokens > 0 ? usage.maxTokens : null;
  if (maxTokens === null || usage.usedPercentage === null) return null;
  const used = Math.round(Math.max(0, Math.min(100, usage.usedPercentage)));
  const label = `${used}% of context used · ${formatContextWindowTokens(usage.usedTokens)} / ${formatContextWindowTokens(maxTokens)} tokens`;
  const ring = (
    <svg viewBox="0 0 16 16" className="size-4 -rotate-90" aria-hidden>
      <circle
        cx="8"
        cy="8"
        r={RADIUS}
        fill="none"
        strokeWidth="2"
        className="stroke-foreground/12"
      />
      <circle
        cx="8"
        cy="8"
        r={RADIUS}
        fill="none"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={`${(used / 100) * CIRCUMFERENCE} ${CIRCUMFERENCE}`}
        className={cn(
          used >= 90
            ? "stroke-destructive"
            : used >= 80
              ? "stroke-warning"
              : "stroke-muted-foreground",
        )}
      />
    </svg>
  );
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          props.onOpen ? (
            <Button
              size="icon-sm"
              variant="ghost"
              aria-label={`${label}. Show what fills it`}
              onClick={props.onOpen}
            />
          ) : (
            <span
              role="img"
              aria-label={label}
              className="inline-flex size-8 items-center justify-center"
            />
          )
        }
      >
        {ring}
      </TooltipTrigger>
      <TooltipPopup>{props.onOpen ? `${label}. Click for details.` : label}</TooltipPopup>
    </Tooltip>
  );
});
