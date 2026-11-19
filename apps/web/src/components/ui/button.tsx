import { mergeProps } from "@base-ui/react/merge-props";
import { useRender } from "@base-ui/react/use-render";
import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";

import { cn } from "~/lib/utils";

const buttonVariants = cva(
  "[--control-icon-color:currentColor] [&_svg]:-mx-0.5 relative inline-flex shrink-0 cursor-pointer select-none items-center justify-center gap-2 whitespace-nowrap rounded-(--control-radius) border font-medium text-base outline-none transition-[color,background-color,border-color,box-shadow,scale] duration-100 [&:hover:not([aria-haspopup]):not(:disabled):not([aria-disabled=true])]:will-change-[scale] [&:active:not([aria-haspopup])]:scale-[0.97] pointer-coarse:after:absolute pointer-coarse:after:size-full pointer-coarse:after:min-h-11 pointer-coarse:after:min-w-11 focus-visible:outline-ring-inset disabled:pointer-events-none disabled:opacity-55 aria-disabled:cursor-not-allowed aria-disabled:opacity-55 sm:text-ui [&_svg:not([class*='text-'])]:text-[var(--control-icon-color)] [&_svg:not([class*='size-'])]:size-4.5 sm:[&_svg:not([class*='size-'])]:size-4 [&_svg]:pointer-events-none [&_svg]:shrink-0",
  {
    defaultVariants: {
      size: "default",
      variant: "default",
    },
    variants: {
      size: {
        compact:
          "h-7 gap-1.5 px-2 text-xs sm:text-xs [&_svg:not([class*='size-'])]:size-3.5 sm:[&_svg:not([class*='size-'])]:size-3.5",
        default: "h-9 px-2.5 sm:h-8",
        icon: "size-9 sm:size-8",
        "icon-lg": "size-10 sm:size-9",
        "icon-micro": "size-5 rounded-sm p-0 [&_svg:not([class*='size-'])]:size-3",
        "icon-tiny": "size-4 p-0 [&_svg:not([class*='size-'])]:size-3",
        "icon-sm": "size-8 sm:size-7",
        "icon-sm-pill": "size-8 rounded-full sm:size-7",
        "icon-xl":
          "size-11 sm:size-10 [&_svg:not([class*='size-'])]:size-5 sm:[&_svg:not([class*='size-'])]:size-4.5",
        "icon-xs":
          "size-7 sm:size-6 not-in-data-[slot=input-group]:[&_svg:not([class*='size-'])]:size-4 sm:not-in-data-[slot=input-group]:[&_svg:not([class*='size-'])]:size-3.5",
        lg: "h-10 px-3 sm:h-9",
        micro:
          "h-5 gap-1 rounded-sm px-1.5 text-2xs sm:text-2xs [&_svg:not([class*='size-'])]:size-3 sm:[&_svg:not([class*='size-'])]:size-3",
        multiline: "min-h-9 px-2.5 py-1.5 whitespace-normal sm:min-h-8",
        sm: "h-8 gap-1.5 px-2 sm:h-7 sm:text-xs sm:[&_svg:not([class*='size-'])]:size-3.5",
        "sm-pill":
          "h-8 gap-1.5 rounded-full px-2.5 sm:h-7 sm:text-xs sm:[&_svg:not([class*='size-'])]:size-3.5",
        "sm-multiline":
          "min-h-8 gap-1.5 px-2 py-1 whitespace-normal sm:min-h-7 sm:text-xs sm:[&_svg:not([class*='size-'])]:size-3.5",
        xl: "h-11 px-4 text-lg sm:h-10 sm:text-base [&_svg:not([class*='size-'])]:size-5 sm:[&_svg:not([class*='size-'])]:size-4.5",
        xs: "h-7 gap-1.5 px-2 text-sm sm:h-6 sm:text-xs [&_svg:not([class*='size-'])]:size-4 sm:[&_svg:not([class*='size-'])]:size-3.5",
      },
      variant: {
        default:
          "border-[color-mix(in_oklch,var(--primary)_80%,black)] bg-primary text-primary-foreground shadow-[inset_0_1px_oklch(100%_0_0/12%),var(--plate-shadow)] [--focus-ring:var(--primary-foreground)] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-(--primary-hover)",
        destructive:
          "border-[color-mix(in_oklch,var(--destructive)_80%,black)] bg-destructive text-white shadow-[inset_0_1px_oklch(100%_0_0/12%),var(--plate-shadow)] [--focus-ring:white] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-[color-mix(in_oklch,var(--destructive)_92%,black)]",
        "destructive-outline":
          "border-border-2 bg-card text-destructive-foreground shadow-[inset_0_1px_var(--highlight-top),var(--plate-shadow)] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:border-destructive/32 [:hover,[data-pressed]]:bg-destructive/4",
        ghost:
          "[--control-icon-color:var(--contrast-muted-foreground)] border-transparent text-foreground [:hover,[data-pressed]]:bg-secondary",
        "ghost-muted":
          "[--control-icon-color:currentColor] border-transparent text-muted-foreground [:hover,[data-pressed]]:bg-secondary [:hover,[data-pressed]]:text-foreground",
        "ghost-destructive":
          "[--control-icon-color:currentColor] border-transparent text-muted-foreground [:hover,[data-pressed]]:bg-secondary [:hover,[data-pressed]]:text-destructive",
        glass:
          "surface-glass [--control-icon-color:var(--contrast-muted-foreground)] rounded-full border-border-2 text-foreground shadow-plate",
        link: "border-transparent underline-offset-4 [:hover,[data-pressed]]:underline",
        "media-close":
          "[--control-icon-color:currentColor] border-transparent bg-black/65 text-white shadow-sm ring-1 ring-white/20 [--focus-ring:white] [:hover,[data-pressed]]:bg-black/80",
        "media-navigation":
          "[--control-icon-color:currentColor] absolute top-1/2 z-20 -translate-y-1/2 border-transparent text-white/90 [--focus-ring:white] [:hover,[data-pressed]]:bg-white/10 [:hover,[data-pressed]]:text-white",
        outline:
          "[--control-icon-color:var(--contrast-muted-foreground)] border-border-2 bg-card text-foreground shadow-[inset_0_1px_var(--highlight-top),var(--plate-shadow)] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-card-hover",
        overlay: "border-transparent bg-black/70 text-white/65 [:hover,[data-pressed]]:bg-black/90",
        scenery:
          "[--control-icon-color:currentColor] inset-shadow-[0_1px_--theme(--color-white/16%)] rounded-full border-transparent bg-(--sky-ridge) text-white/92 [--focus-ring:white] [:hover,[data-pressed]]:inset-shadow-none [:hover,[data-pressed]]:bg-(--sky-body) [:hover,[data-pressed]]:text-(--sky-ink) [:hover,[data-pressed]]:shadow-[0_0_10px_var(--sky-body-glow)]",
        secondary:
          "border-transparent bg-secondary text-secondary-foreground [:hover,[data-pressed]]:bg-secondary-hover",
        "warning-outline":
          "border-warning/32 bg-warning-surface text-warning-foreground shadow-plate [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:border-warning/40 [:hover,[data-pressed]]:bg-warning/16 dark:[:hover,[data-pressed]]:bg-warning/24",
      },
    },
  },
);

type ButtonVariant = NonNullable<VariantProps<typeof buttonVariants>["variant"]>;
type ButtonSize = NonNullable<VariantProps<typeof buttonVariants>["size"]>;

interface ButtonProps extends useRender.ComponentProps<"button"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

function Button({ className, variant, size, render, ...props }: ButtonProps) {
  const typeValue: React.ButtonHTMLAttributes<HTMLButtonElement>["type"] = render
    ? undefined
    : "button";

  const defaultProps = {
    className: cn(buttonVariants({ className, size, variant })),
    "data-slot": "button",
    type: typeValue,
  };

  return useRender({
    defaultTagName: "button",
    props: mergeProps<"button">(defaultProps, props),
    render,
  });
}

export { Button, buttonVariants, type ButtonSize, type ButtonVariant };

const inlineButtonVariants = cva(
  "inline-flex shrink-0 cursor-pointer items-center gap-0.5 font-medium underline-offset-2 [text-align:var(--inline-button-text-align,center)] [white-space:var(--inline-button-white-space,nowrap)] hover:underline focus-visible:outline-1 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-55",
  {
    defaultVariants: { tone: "default" },
    variants: {
      touchTarget: {
        padded: "pointer-coarse:min-h-11 pointer-coarse:px-1.5",
      },
      tone: {
        default: "text-foreground",
        muted: "text-muted-foreground hover:text-foreground",
        destructive: "text-destructive/80 hover:text-destructive",
        picker:
          "gap-1 text-foreground decoration-from-font underline-offset-4 data-popup-open:underline",
      },
    },
  },
);

export function InlineButton({
  className,
  tone,
  touchTarget,
  render,
  ...props
}: useRender.ComponentProps<"button"> & VariantProps<typeof inlineButtonVariants>) {
  const defaultProps = {
    className: cn(inlineButtonVariants({ tone, touchTarget }), className),
    "data-slot": "inline-button",
    type: render ? undefined : ("button" as const),
  };
  return useRender({
    defaultTagName: "button",
    props: mergeProps<"button">(defaultProps, props),
    render,
  });
}
