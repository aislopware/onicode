import { Tabs as TabsPrimitive } from "@base-ui/react/tabs";

import { cn } from "~/lib/utils";

function Tabs({ className, ...props }: TabsPrimitive.Root.Props) {
  return <TabsPrimitive.Root className={cn("min-w-0", className)} data-slot="tabs" {...props} />;
}

/** Section tabs: one bar slides to the active tab. The bar it sits in draws the baseline. */
function TabsList({ className, children, ...props }: TabsPrimitive.List.Props) {
  return (
    <TabsPrimitive.List
      className={cn("relative flex min-w-0 items-stretch gap-5", className)}
      data-slot="tabs-list"
      {...props}
    >
      {children}
      <TabsPrimitive.Indicator
        // A keyboard move lands at once: the bar must not trail the focus ring.
        className="pointer-events-none absolute bottom-0 left-0 h-0.5 w-(--active-tab-width) translate-x-(--active-tab-left) rounded-full bg-foreground transition-[translate,width] duration-200 ease-out-quint motion-reduce:transition-none in-[[data-slot=tabs-list]:has(:focus-visible)]:transition-none"
        data-slot="tabs-indicator"
      />
    </TabsPrimitive.List>
  );
}

function TabsTab({ className, ...props }: TabsPrimitive.Tab.Props) {
  return (
    <TabsPrimitive.Tab
      className={cn(
        "relative inline-flex h-10 shrink-0 cursor-pointer items-center justify-center gap-1.5 rounded-sm px-0.5 text-base whitespace-nowrap text-muted-foreground outline-none transition-colors duration-100 select-none hover:text-foreground focus-visible:after:pointer-events-none focus-visible:after:absolute focus-visible:after:-inset-x-1.5 focus-visible:after:inset-y-1.5 focus-visible:after:rounded-sm focus-visible:after:outline-ring-inset data-active:text-foreground data-disabled:pointer-events-none data-disabled:opacity-55 sm:h-9 sm:text-ui [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
        className,
      )}
      data-slot="tabs-tab"
      {...props}
    />
  );
}

export { Tabs, TabsList, TabsTab };
