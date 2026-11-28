import type { Toggle as TogglePrimitive } from "@base-ui/react/toggle";
import { ToggleGroup as ToggleGroupPrimitive } from "@base-ui/react/toggle-group";
import type { VariantProps } from "class-variance-authority";
import * as React from "react";

import { observeResize } from "~/lib/observeResize";
import { cn } from "~/lib/utils";
import { Toggle as ToggleComponent, type toggleVariants } from "~/components/ui/toggle";

const ToggleGroupContext = React.createContext<
  VariantProps<typeof toggleVariants> & { sliding: boolean }
>({
  size: "default",
  sliding: false,
  variant: "default",
});

function ToggleGroup({
  className,
  variant = "segmented",
  size: sizeProp,
  orientation = "horizontal",
  multiple,
  children,
  ...props
}: ToggleGroupPrimitive.Props & VariantProps<typeof toggleVariants>) {
  const size =
    sizeProp === undefined ? (variant === "segmented" ? "segmented" : "default") : sizeProp;
  // One surface can mark one item: a group that allows several keeps a surface on each.
  const sliding = variant === "segmented" && !multiple;
  const value = { size, sliding, variant };
  return (
    <ToggleGroupPrimitive
      className={cn(
        "flex w-fit *:focus-visible:z-10",
        orientation === "horizontal"
          ? "*:pointer-coarse:after:min-w-auto"
          : "*:pointer-coarse:after:min-h-auto",
        variant === "segmented"
          ? "relative isolate gap-0.5 rounded-lg bg-secondary p-0.5 dark:bg-white/4.5"
          : variant === "default"
            ? "gap-0.5"
            : orientation === "horizontal"
              ? "*:not-first:rounded-s-none *:not-last:rounded-e-none *:not-first:border-s-0"
              : "flex-col *:not-first:rounded-t-none *:not-last:rounded-b-none *:not-first:border-t-0",
        className,
      )}
      data-segmented={sliding || undefined}
      data-size={size}
      data-slot="toggle-group"
      data-variant={variant}
      multiple={multiple}
      orientation={orientation}
      {...props}
    >
      <ToggleGroupContext value={value}>{children}</ToggleGroupContext>
      {sliding ? <SegmentedIndicator /> : null}
    </ToggleGroupPrimitive>
  );
}

/**
 * The selected surface of a segmented group: one element that slides to the selected item. Place
 * it last in a `relative isolate` track marked `data-segmented`; `selected` finds the item (radio groups mark theirs
 * `data-checked`).
 */
function SegmentedIndicator({ selected = "data-pressed" }: { selected?: string }) {
  const ref = React.useRef<HTMLSpanElement>(null);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- the pressed item's box is known only from layout
  React.useLayoutEffect(() => {
    const indicator = ref.current;
    const group = indicator?.parentElement;
    if (!indicator || !group) return;
    const place = () => {
      const pressed = group.querySelector<HTMLElement>(`[${selected}]`);
      indicator.hidden = pressed === null;
      if (pressed === null) return;
      const from = group.getBoundingClientRect();
      const to = pressed.getBoundingClientRect();
      // A dialog opens scaled: rects are in screen pixels, the indicator is laid out unscaled.
      const laidOut = getComputedStyle(group);
      const scaleX = from.width / Number.parseFloat(laidOut.width) || 1;
      const scaleY = from.height / Number.parseFloat(laidOut.height) || 1;
      const left = (to.left - from.left) / scaleX - group.clientLeft;
      const top = (to.top - from.top) / scaleY - group.clientTop;
      indicator.style.setProperty("--pressed-left", `${left}px`);
      indicator.style.setProperty("--pressed-top", `${top}px`);
      indicator.style.setProperty("--pressed-width", `${to.width / scaleX}px`);
      indicator.style.setProperty("--pressed-height", `${to.height / scaleY}px`);
    };
    place();
    // Read the placed box before motion is allowed, so the first placement does not slide in.
    void indicator.offsetWidth;
    indicator.dataset.placed = "";
    const pressedChanges = new MutationObserver(place);
    pressedChanges.observe(group, {
      attributeFilter: [selected],
      attributes: true,
      characterData: true,
      childList: true,
      subtree: true,
    });
    const stopObservingSize = observeResize(group, place);
    return () => {
      pressedChanges.disconnect();
      stopObservingSize();
    };
  }, [selected]);

  return (
    <span
      aria-hidden
      className="pointer-events-none absolute top-0 left-0 -z-1 h-(--pressed-height) w-(--pressed-width) translate-x-(--pressed-left) translate-y-(--pressed-top) rounded-(--control-radius) border border-transparent bg-card shadow-raised transition-[translate,width,height] duration-200 ease-out-quint not-data-placed:transition-none motion-reduce:transition-none in-[[data-segmented]:has(:focus-visible)]:transition-none dark:bg-white/8"
      data-slot="toggle-group-indicator"
      ref={ref}
    />
  );
}

function Toggle({
  className,
  children,
  variant,
  size,
  ...props
}: TogglePrimitive.Props & VariantProps<typeof toggleVariants>) {
  const context = React.use(ToggleGroupContext);

  const resolvedVariant = variant ?? context.variant;
  const resolvedSize = size ?? context.size;

  return (
    <ToggleComponent
      // The group's indicator is the selected surface here, so the item keeps only its text colour.
      className={cn(
        context.sliding &&
          resolvedVariant === "segmented" &&
          "data-pressed:bg-transparent data-pressed:shadow-none data-pressed:hover:bg-transparent dark:data-pressed:bg-transparent dark:data-pressed:hover:bg-transparent",
        className,
      )}
      data-size={resolvedSize}
      data-variant={resolvedVariant}
      size={resolvedSize}
      variant={resolvedVariant}
      {...props}
    >
      {children}
    </ToggleComponent>
  );
}

export { ToggleGroup, Toggle, Toggle as ToggleGroupItem, SegmentedIndicator };
