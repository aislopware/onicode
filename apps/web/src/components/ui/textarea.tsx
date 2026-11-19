import { Field as FieldPrimitive } from "@base-ui/react/field";
import { mergeProps } from "@base-ui/react/merge-props";
import type * as React from "react";

import { cn } from "~/lib/utils";

type TextareaProps = React.ComponentProps<"textarea"> & {
  size?: "sm" | "default" | "lg" | number;
  unstyled?: boolean;
};

function Textarea({
  className,
  size = "default",
  unstyled = false,
  rows,
  ...props
}: TextareaProps) {
  return (
    <span
      className={
        cn(
          !unstyled &&
            "relative inline-flex w-full rounded-(--control-radius) border border-input bg-card text-base text-foreground transition-colors duration-100 has-focus-visible:outline-ring-inset has-aria-invalid:border-destructive has-aria-invalid:[--focus-ring:var(--destructive)] has-autofill:bg-foreground/4 has-disabled:opacity-55 sm:text-ui dark:has-autofill:bg-foreground/8",
          className,
        ) || undefined
      }
      data-size={size}
      data-slot="textarea-control"
    >
      <FieldPrimitive.Control
        render={(defaultProps) => (
          <textarea
            className={cn(
              "field-sizing-content max-h-64 w-full rounded-[inherit] px-2.5 py-1.5 outline-none placeholder:text-placeholder wrap-anywhere",
              rows === undefined && "min-h-17.5 max-sm:min-h-20.5",
              size === "sm" && "px-2 py-1",
              size === "sm" && rows === undefined && "min-h-16.5 max-sm:min-h-19.5",
              size === "lg" && "py-2",
              size === "lg" && rows === undefined && "min-h-18.5 max-sm:min-h-21.5",
            )}
            // field-sizing: content ignores `rows`, so an explicit row count sets the minimum height.
            style={
              rows === undefined
                ? undefined
                : {
                    minHeight: `calc(${rows}lh + 2 * var(--spacing) * ${size === "sm" ? 1 : size === "lg" ? 2 : 1.5})`,
                  }
            }
            data-slot="textarea"
            rows={rows}
            {...mergeProps(defaultProps, props)}
          />
        )}
      />
    </span>
  );
}

export { Textarea, type TextareaProps };
