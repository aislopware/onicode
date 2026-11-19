import { Checkbox as CheckboxPrimitive } from "@base-ui/react/checkbox";

import { cn } from "~/lib/utils";

const rootClassName =
  "relative inline-flex size-4.5 shrink-0 items-center justify-center rounded-sm border border-border-3 bg-card outline-none transition-colors duration-100 focus-visible:outline-ring-inset aria-invalid:border-destructive data-disabled:opacity-55 sm:size-4 [[data-checked],[data-indeterminate]]:border-transparent [[data-checked],[data-indeterminate]]:bg-primary [[data-checked],[data-indeterminate]]:[--focus-ring:var(--primary-foreground)]";

const indicatorClassName =
  "flex items-center justify-center text-primary-foreground data-unchecked:hidden";

const variantClassNames = {
  default: "",
  contrast: "border-foreground/55",
};

type CheckboxVariant = keyof typeof variantClassNames;

function Checkbox({
  className,
  variant = "default",
  ...props
}: CheckboxPrimitive.Root.Props & { variant?: CheckboxVariant }) {
  return (
    <CheckboxPrimitive.Root
      className={cn(rootClassName, variantClassNames[variant], className)}
      data-slot="checkbox"
      {...props}
    >
      {/* Explicit glyph colors resist row icon-muting rules. */}
      <CheckboxPrimitive.Indicator
        className={indicatorClassName}
        data-slot="checkbox-indicator"
        render={(props, state) => (
          <span {...props}>
            {state.indeterminate ? (
              <svg
                className="size-3.5 text-current sm:size-3"
                fill="none"
                height="24"
                stroke="currentColor"
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth="3"
                viewBox="0 0 24 24"
                width="24"
                xmlns="http://www.w3.org/2000/svg"
              >
                <path d="M5.252 12h13.496" />
              </svg>
            ) : (
              <svg
                className="size-3.5 text-current sm:size-3"
                fill="none"
                height="24"
                stroke="currentColor"
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth="3"
                viewBox="0 0 24 24"
                width="24"
                xmlns="http://www.w3.org/2000/svg"
              >
                <path d="M5.252 12.7 10.2 18.63 18.748 5.37" />
              </svg>
            )}
          </span>
        )}
      />
    </CheckboxPrimitive.Root>
  );
}

// Checkbox's look without its hidden input, for a row that is itself the button.
function CheckboxMark({
  checked,
  variant = "default",
}: {
  checked: boolean;
  variant?: CheckboxVariant;
}) {
  const state = checked ? { "data-checked": "" } : { "data-unchecked": "" };
  return (
    <span
      aria-hidden
      className={cn(rootClassName, variantClassNames[variant])}
      data-slot="checkbox"
      {...state}
    >
      <span className={indicatorClassName} data-slot="checkbox-indicator" {...state}>
        <svg
          className="size-3.5 text-current sm:size-3"
          fill="none"
          height="24"
          stroke="currentColor"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="3"
          viewBox="0 0 24 24"
          width="24"
          xmlns="http://www.w3.org/2000/svg"
        >
          <path d="M5.252 12.7 10.2 18.63 18.748 5.37" />
        </svg>
      </span>
    </span>
  );
}

export { Checkbox, CheckboxMark };
