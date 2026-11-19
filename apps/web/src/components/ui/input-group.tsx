import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";

import { cn } from "~/lib/utils";
import { Input, type InputProps } from "~/components/ui/input";

const inputGroupVariants = cva(
  "relative inline-flex w-full min-w-0 items-center rounded-(--control-radius) border text-base text-foreground transition-colors duration-100 has-[textarea]:h-auto has-data-[align=block-end]:h-auto has-data-[align=block-start]:h-auto has-data-[align=block-end]:flex-col has-data-[align=block-start]:flex-col has-[input[aria-invalid=true],textarea[aria-invalid=true]]:border-destructive has-[input[aria-invalid=true],textarea[aria-invalid=true]]:[--focus-ring:var(--destructive)] has-autofill:bg-foreground/4 has-[input:disabled,textarea:disabled]:opacity-55 sm:text-ui dark:has-autofill:bg-foreground/8 has-data-[align=inline-start]:**:[[data-size=sm]_input]:ps-1.5 has-data-[align=inline-end]:**:[[data-size=sm]_input]:pe-1.5 *:[[data-slot=input-control],[data-slot=textarea-control]]:contents has-[[data-align=block-start],[data-align=block-end]]:**:[input]:h-auto has-data-[align=inline-start]:**:[input]:ps-2 has-data-[align=inline-end]:**:[input]:pe-2 has-data-[align=block-end]:**:[input]:pt-1.5 has-data-[align=block-start]:**:[input]:pb-1.5 **:[textarea]:min-h-20.5 **:[textarea]:resize-none **:[textarea]:py-3 **:[textarea]:max-sm:min-h-23.5 **:[textarea_button]:rounded-[calc(var(--control-radius)-1px)]",
  {
    defaultVariants: {
      variant: "default",
      size: "default",
    },
    variants: {
      size: {
        default: "h-9 sm:h-8",
        lg: "h-10 sm:h-9",
      },
      variant: {
        default:
          "border-input bg-card has-[input:focus-visible,textarea:focus-visible]:outline-ring-inset",
        popover:
          "dropdown-glass rounded-2xl has-[input:focus-visible,textarea:focus-visible]:border-[color-mix(in_srgb,var(--contrast-foreground)_18%,transparent)]",
        ghost:
          "border-transparent bg-transparent hover:bg-secondary has-[input:focus-visible,textarea:focus-visible]:bg-card has-[input:focus-visible,textarea:focus-visible]:outline-ring-inset",
      },
    },
  },
);

function InputGroup({
  className,
  variant,
  size,
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof inputGroupVariants>) {
  return (
    <div
      className={cn(inputGroupVariants({ variant, size }), className)}
      data-slot="input-group"
      role="group"
      {...props}
    />
  );
}

const inputGroupAddonVariants = cva(
  "[&_svg]:-mx-0.5 flex h-auto cursor-text select-none items-center justify-center gap-2 leading-none [&>kbd]:rounded-[calc(var(--radius)-5px)] in-[[data-slot=input-group]:has([data-slot=input-control],[data-slot=textarea-control])]:[&_svg:not([class*='size-'])]:size-4.5 sm:in-[[data-slot=input-group]:has([data-slot=input-control],[data-slot=textarea-control])]:[&_svg:not([class*='size-'])]:size-4 not-has-[button]:**:[svg:not([class*='opacity-'])]:opacity-80",
  {
    defaultVariants: {
      align: "inline-start",
    },
    compoundVariants: [
      {
        align: "inline-end",
        inset: "pill",
        className: "has-[>button]:me-0 pe-1 [[data-size=sm]+&]:pe-1 gap-0.5",
      },
      {
        align: "inline-start",
        inset: "pill",
        className: "has-[>button]:ms-0 ps-1 [[data-size=sm]+&]:ps-1 gap-0.5",
      },
    ],
    variants: {
      inset: {
        default: null,
        pill: "[&_button]:rounded-full",
      },
      align: {
        "block-end":
          "order-last w-full justify-start px-2.5 pb-2.5 [.border-t]:pt-2.5 [[data-size=sm]+&]:px-2",
        "block-start":
          "order-first w-full justify-start px-2.5 pt-2.5 [.border-b]:pb-2.5 [[data-size=sm]+&]:px-2",
        "inline-end":
          "has-[>:last-child[data-slot=badge]]:-me-1.5 has-[>button]:-me-2 order-last pe-2.5 has-[>kbd:last-child]:me-[-0.35rem] [[data-size=sm]+&]:pe-2",
        "inline-start":
          "has-[>:last-child[data-slot=badge]]:-ms-1.5 has-[>button]:-ms-2 order-first ps-2.5 has-[>kbd:last-child]:ms-[-0.35rem] [[data-size=sm]+&]:ps-2",
      },
    },
  },
);

function InputGroupAddon({
  className,
  align = "inline-start",
  inset,
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof inputGroupAddonVariants>) {
  return (
    <div
      className={cn(inputGroupAddonVariants({ align, inset }), className)}
      data-align={align}
      data-slot="input-group-addon"
      onMouseDown={(e) => {
        if (e.button !== 0) return;
        const target = e.target as HTMLElement;
        const isInteractive = target.closest(
          "button, a, input, select, textarea, [role='button'], [role='combobox'], [role='listbox'], [data-slot='select-trigger']",
        );
        if (isInteractive) return;
        e.preventDefault();
        const parent = e.currentTarget.parentElement;
        const input = parent?.querySelector<HTMLInputElement | HTMLTextAreaElement>(
          "input, textarea",
        );
        if (input && !parent?.querySelector("input:focus, textarea:focus")) {
          input.focus();
        }
      }}
      {...props}
    />
  );
}

function InputGroupInput({ className, ...props }: InputProps) {
  return <Input className={className} unstyled {...props} />;
}

export { InputGroup, InputGroupAddon, InputGroupInput };
