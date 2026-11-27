import { Toggle as TogglePrimitive } from "@base-ui/react/toggle";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "~/lib/utils";

const toggleVariants = cva(
  "[&_svg]:-mx-0.5 relative inline-flex shrink-0 cursor-pointer select-none items-center justify-center gap-2 whitespace-nowrap rounded-(--control-radius) border font-medium text-base text-foreground outline-none transition-[color,background-color,border-color,box-shadow] duration-100 pointer-coarse:after:absolute pointer-coarse:after:size-full pointer-coarse:after:min-h-11 pointer-coarse:after:min-w-11 hover:bg-secondary focus-visible:outline-ring-inset disabled:pointer-events-none disabled:opacity-55 aria-disabled:cursor-not-allowed aria-disabled:opacity-55 aria-disabled:hover:bg-transparent data-pressed:bg-(--contrast-accent) data-pressed:hover:bg-accent-hover sm:text-ui [&_svg:not([class*='opacity-'])]:opacity-80 [&_svg:not([class*='size-'])]:size-4.5 sm:[&_svg:not([class*='size-'])]:size-4 [&_svg]:pointer-events-none [&_svg]:shrink-0",
  {
    defaultVariants: {
      size: "default",
      variant: "default",
    },
    variants: {
      size: {
        compact:
          "h-7 min-w-7 px-1 text-xs sm:text-xs [&_svg:not([class*='size-'])]:size-3.5 sm:[&_svg:not([class*='size-'])]:size-3.5",
        default: "h-9 min-w-9 px-2 sm:h-8 sm:min-w-8",
        lg: "h-10 min-w-10 px-2.5 sm:h-9 sm:min-w-9",
        segmented: "h-6 min-w-7 rounded-(--control-radius) px-2.5 text-xs sm:text-xs",
        sm: "h-8 min-w-8 px-1.5 sm:h-7 sm:min-w-7",
        xs: "h-7 min-w-7 px-1 sm:h-6 sm:min-w-6",
      },
      variant: {
        default: "border-transparent",
        pill: "rounded-full border-transparent px-3 font-normal text-muted-foreground data-pressed:text-foreground",
        ghost:
          "border-transparent disabled:text-muted-foreground disabled:opacity-100 disabled:[&_svg]:opacity-100 aria-disabled:text-muted-foreground aria-disabled:opacity-100 aria-disabled:[&_svg]:opacity-100",
        outline:
          "border-border-2 bg-card shadow-[inset_0_1px_var(--highlight-top),var(--plate-shadow)] hover:bg-card-hover [:disabled,:active,[data-pressed]]:shadow-none",
        segmented:
          "border-transparent text-muted-foreground hover:bg-transparent hover:text-foreground data-pressed:bg-card data-pressed:text-foreground data-pressed:shadow-raised data-pressed:hover:bg-card dark:data-pressed:bg-white/8 dark:data-pressed:hover:bg-white/8",
      },
    },
  },
);

function Toggle({
  className,
  variant,
  size,
  ...props
}: TogglePrimitive.Props & VariantProps<typeof toggleVariants>) {
  return (
    <TogglePrimitive
      className={cn(toggleVariants({ className, size, variant }))}
      data-slot="toggle"
      {...props}
    />
  );
}

export { Toggle, toggleVariants };
