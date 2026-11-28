import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import {
  createContext,
  use,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
  type RefObject,
} from "react";

import { fitBoxToText } from "~/lib/fitBoxToText";
import { cn } from "~/lib/utils";
import { useNativeTitlebarHeight } from "~/lib/windowControlsOverlay";

import { Kbd } from "./kbd";

// Base UI's default, restated because a padding object zeroes the sides it omits.
const COLLISION_PADDING = 5;

/** How long the pointer rests before the first tooltip opens. */
const TOOLTIP_DELAY_MS = 300;
/** After one closes, the next opens at once: the reader is scanning, not waiting. */
const TOOLTIP_WARM_MS = 300;

function TooltipProvider({
  delay = TOOLTIP_DELAY_MS,
  timeout = TOOLTIP_WARM_MS,
  ...props
}: TooltipPrimitive.Provider.Props) {
  return <TooltipPrimitive.Provider delay={delay} timeout={timeout} {...props} />;
}

type TooltipActionsRef = RefObject<TooltipPrimitive.Root.Actions | null>;

// The compiler freezes context values; update hovered tooltips through calls instead of shared ref writes.
function createTooltipHoverTracker() {
  let hovered: { trigger: HTMLElement; owner: object; close: () => void } | null = null;
  return {
    hover(trigger: HTMLElement, owner: object, close: () => void) {
      hovered = { trigger, owner, close };
    },
    release(owner: object) {
      if (hovered?.owner === owner) hovered = null;
    },
    dismissUnfocused() {
      const tooltip = hovered;
      if (!tooltip) return;
      const focused = tooltip.trigger.ownerDocument.activeElement;
      if (focused && tooltip.trigger.contains(focused) && focused.matches(":focus-visible")) {
        return;
      }
      hovered = null;
      tooltip.close();
    },
  };
}

type TooltipHoverTracker = ReturnType<typeof createTooltipHoverTracker>;
const TooltipHoverContext = createContext<TooltipActionsRef | null>(null);
const TooltipScrollContext = createContext<{
  tracker: TooltipHoverTracker;
  holdPositionOnExit: boolean;
} | null>(null);

function TooltipScrollDismissArea({
  holdPositionOnExit = false,
  onScrollCapture,
  ...props
}: ComponentProps<"div"> & { holdPositionOnExit?: boolean }) {
  const [tracker] = useState(createTooltipHoverTracker);
  return (
    <TooltipScrollContext value={{ tracker, holdPositionOnExit }}>
      <div
        {...props}
        onScrollCapture={(event) => {
          onScrollCapture?.(event);
          tracker.dismissUnfocused();
        }}
      />
    </TooltipScrollContext>
  );
}

function Tooltip<Payload>(props: TooltipPrimitive.Root.Props<Payload>) {
  const tracker = useTooltipScrollTracker();
  const localActionsRef = useRef<TooltipPrimitive.Root.Actions | null>(null);
  const actionsRef = props.actionsRef ?? localActionsRef;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- releases this tooltip from the scroll tracker on unmount
  useEffect(() => () => tracker?.release(actionsRef), [actionsRef, tracker]);

  if (!tracker) return <TooltipPrimitive.Root {...props} />;
  return (
    <TooltipHoverContext value={actionsRef}>
      <TooltipPrimitive.Root
        {...props}
        actionsRef={actionsRef}
        onOpenChange={(open, details) => {
          props.onOpenChange?.(open, details);
          if (!open && !details.isCanceled) tracker.release(actionsRef);
        }}
      />
    </TooltipHoverContext>
  );
}

function TooltipTrigger(props: TooltipPrimitive.Trigger.Props) {
  const tracker = useTooltipScrollTracker();
  const actionsRef = use(TooltipHoverContext);
  if (!tracker || !actionsRef) {
    return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />;
  }
  return (
    <TooltipPrimitive.Trigger
      data-slot="tooltip-trigger"
      {...props}
      onMouseEnter={(event) => {
        props.onMouseEnter?.(event);
        // Base UI also cancels delayed hover opens through this action.
        tracker.hover(event.currentTarget, actionsRef, () => actionsRef.current?.close());
      }}
    />
  );
}

function TooltipPositioner(props: TooltipPrimitive.Positioner.Props) {
  const titlebarHeight = useNativeTitlebarHeight();
  const holdPositionOnExit = use(TooltipScrollContext)?.holdPositionOnExit;
  return (
    <TooltipPrimitive.Positioner
      className={cn(
        "pointer-events-none z-[140] max-w-(--available-width) transition-[top,left,right,bottom,transform] data-instant:transition-none motion-reduce:transition-none",
        // Defer repositioning until the popup's fade finishes when its trigger scrolls away.
        holdPositionOnExit && "data-closed:delay-(--default-transition-duration)",
      )}
      collisionPadding={{
        top: COLLISION_PADDING + titlebarHeight,
        right: COLLISION_PADDING,
        bottom: COLLISION_PADDING,
        left: COLLISION_PADDING,
      }}
      data-slot="tooltip-positioner"
      {...props}
    />
  );
}

function TooltipPopup({
  className,
  align = "center",
  sideOffset = 4,
  side = "top",
  variant = "default",
  anchor,
  collisionAvoidance,
  shortcut,
  description,
  children,
  ...props
}: TooltipPrimitive.Popup.Props & {
  align?: TooltipPrimitive.Positioner.Props["align"];
  collisionAvoidance?: TooltipPrimitive.Positioner.Props["collisionAvoidance"];
  side?: TooltipPrimitive.Positioner.Props["side"];
  sideOffset?: TooltipPrimitive.Positioner.Props["sideOffset"];
  variant?: "default" | "glass" | "code";
  anchor?: TooltipPrimitive.Positioner.Props["anchor"];
  /** The key combination for the action, shown as a key chip after the label. */
  shortcut?: string | null | undefined;
  /** A sentence under the label; the children become its title. */
  description?: ReactNode;
}) {
  return (
    <TooltipPrimitive.Portal>
      <TooltipPositioner
        align={align}
        anchor={anchor}
        side={side}
        sideOffset={sideOffset}
        {...(collisionAvoidance ? { collisionAvoidance } : {})}
      >
        <TooltipPrimitive.Popup
          className={cn(
            "relative flex origin-(--transform-origin) rounded-md font-medium text-popover-foreground text-xs transition-[opacity,scale] duration-[125ms] ease-out-quint will-change-[scale] motion-safe:data-ending-style:scale-[0.97] data-ending-style:opacity-0 data-ending-style:duration-100 motion-safe:data-starting-style:scale-[0.97] data-starting-style:opacity-0 data-instant:duration-0",
            variant === "glass"
              ? "dropdown-glass shadow-popup dark:shadow-popup-dark"
              : "border border-border-2 bg-popover shadow-popup dark:shadow-popup-dark",
            variant === "code"
              ? "max-w-120 wrap-anywhere text-left font-mono text-2xs leading-relaxed"
              : "max-w-80 wrap-anywhere whitespace-normal text-balance leading-snug",
            description !== undefined && "max-w-64",
            className,
          )}
          data-slot="tooltip-popup"
          {...props}
        >
          <TooltipViewport
            className={cn(
              "min-w-0 px-2 py-1",
              shortcut && "flex items-center gap-2",
              description !== undefined && "py-1.5 text-left",
            )}
          >
            {shortcut || description !== undefined ? (
              <span className="min-w-0">{children}</span>
            ) : (
              children
            )}
            {shortcut ? <Kbd className="-me-1 h-4 min-w-4 shrink-0">{shortcut}</Kbd> : null}
            {description !== undefined ? (
              <TooltipDescription>{description}</TooltipDescription>
            ) : null}
          </TooltipViewport>
        </TooltipPrimitive.Popup>
      </TooltipPositioner>
    </TooltipPrimitive.Portal>
  );
}

function TooltipViewport({ className, children }: { className: string; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- measures the rendered lines after each content change so wrapped text keeps even padding
  useLayoutEffect(() => {
    if (ref.current) fitBoxToText(ref.current);
  });
  return (
    <div ref={ref} className={className} data-slot="tooltip-viewport">
      {children}
    </div>
  );
}

/** Body text of a tooltip: the regular-weight line under a medium title. */
function TooltipDescription({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      className={cn("mt-0.5 font-normal text-muted-foreground", className)}
      data-slot="tooltip-description"
      {...props}
    />
  );
}

/** One line of a tooltip that lists several actions, each with its own key chip. */
function TooltipRow({
  shortcut,
  className,
  children,
}: {
  shortcut?: string | null | undefined;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={cn("flex items-center justify-between gap-3 not-first:mt-1", className)}
      data-slot="tooltip-row"
    >
      <span className="min-w-0">{children}</span>
      {shortcut ? <Kbd className="-me-1 h-4 min-w-4 shrink-0">{shortcut}</Kbd> : null}
    </div>
  );
}

/** The enclosing TooltipScrollDismissArea's tracker, for triggers that bypass TooltipTrigger. */
const useTooltipScrollTracker = () => use(TooltipScrollContext)?.tracker ?? null;

export {
  TOOLTIP_DELAY_MS,
  TOOLTIP_WARM_MS,
  TooltipProvider,
  Tooltip,
  TooltipTrigger,
  TooltipPopup,
  TooltipDescription,
  TooltipRow,
  TooltipScrollDismissArea,
  useTooltipScrollTracker,
};
