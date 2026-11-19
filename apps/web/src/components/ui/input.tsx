import { Input as InputPrimitive } from "@base-ui/react/input";
import type * as React from "react";

import { cn } from "~/lib/utils";

type InputProps = Omit<InputPrimitive.Props & React.RefAttributes<HTMLInputElement>, "size"> & {
  size?: "sm" | "compact" | "default" | "lg" | number;
  font?: "default" | "mono";
  unstyled?: boolean;
  nativeInput?: boolean;
};

function Input({
  className,
  size = "default",
  font = "default",
  unstyled = false,
  nativeInput = false,
  ...props
}: InputProps) {
  const inputClassName = cn(
    "w-full min-w-0 rounded-[inherit] px-2.5 outline-none placeholder:text-placeholder [transition:background-color_5000000s_ease-in-out_0s]",
    size === "compact" && "px-2 text-xs",
    size === "sm" && "px-2",
    // Without the bordered box around it, the input sets its own height.
    unstyled &&
      (size === "compact"
        ? "h-7 leading-7"
        : size === "sm"
          ? "h-7.5 leading-7.5 sm:h-6.5 sm:leading-6.5"
          : size === "lg"
            ? "h-9.5 leading-9.5 sm:h-8.5 sm:leading-8.5"
            : "h-8.5 leading-8.5 sm:h-7.5 sm:leading-7.5"),
    props.type === "search" &&
      "[&::-webkit-search-cancel-button]:appearance-none [&::-webkit-search-decoration]:appearance-none [&::-webkit-search-results-button]:appearance-none [&::-webkit-search-results-decoration]:appearance-none",
    props.type === "number" &&
      "[appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none",
    props.type === "file" &&
      "text-muted-foreground file:me-3 file:bg-transparent file:font-medium file:text-foreground file:text-sm",
  );
  let inputElement: React.ReactElement;

  if (nativeInput) {
    const { style, onValueChange: _onValueChange, ...nativeInputProps } = props;
    const nativeStyle = typeof style === "function" ? undefined : style;

    inputElement = (
      <input
        className={inputClassName}
        data-slot="input"
        size={typeof size === "number" ? size : undefined}
        style={nativeStyle}
        {...(nativeInputProps as React.ComponentProps<"input">)}
      />
    );
  } else {
    inputElement = (
      <InputPrimitive
        className={inputClassName}
        data-slot="input"
        size={typeof size === "number" ? size : undefined}
        {...props}
      />
    );
  }

  return (
    <span
      className={
        cn(
          !unstyled &&
            "relative inline-flex w-full rounded-(--control-radius) border border-input bg-card text-base text-foreground transition-colors duration-100 has-focus-visible:outline-ring-inset has-aria-invalid:border-destructive has-aria-invalid:[--focus-ring:var(--destructive)] has-autofill:bg-foreground/4 has-disabled:opacity-55 sm:text-ui dark:has-autofill:bg-foreground/8",
          !unstyled &&
            (size === "compact"
              ? "h-7"
              : size === "sm"
                ? "h-8 sm:h-7"
                : size === "lg"
                  ? "h-10 sm:h-9"
                  : "h-9 sm:h-8"),
          font === "mono" && "font-mono tabular-nums",
          className,
        ) || undefined
      }
      data-size={size}
      data-slot="input-control"
    >
      {inputElement}
    </span>
  );
}

export { Input, type InputProps };
