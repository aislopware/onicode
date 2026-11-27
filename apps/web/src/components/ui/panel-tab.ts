import { cva } from "class-variance-authority";

/** A tab in a strip of open panes. The strip sits on the canvas, so the open tab is a wash, not a plate. */
export const panelTabVariants = cva(
  "relative flex h-7 shrink-0 cursor-pointer items-center rounded-(--control-radius) border border-transparent text-xs font-medium transition-[color,background-color] duration-100 has-[.panel-tab-label:focus-visible]:outline-ring-inset",
  {
    variants: {
      active: {
        true: "bg-accent text-foreground",
        false: "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
      },
      // A tab in a multi-tab selection: a flat theme wash, so the range reads apart from the open tab.
      selected: {
        true: "bg-primary/15 text-foreground hover:bg-primary/20",
        false: "",
      },
    },
  },
);
