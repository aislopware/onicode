import { Switch as SwitchPrimitive } from "@base-ui/react/switch";

import { cn } from "~/lib/utils";

function Switch({
  className,
  size = "default",
  mixed = false,
  ...props
}: SwitchPrimitive.Root.Props & { size?: "default" | "sm"; mixed?: boolean }) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "inline-flex h-[calc(var(--thumb-size)+4px)] w-[calc(var(--thumb-size)*2+4px)] shrink-0 cursor-pointer items-center rounded-full border border-border-2 bg-(--switch-track) p-[1.5px] outline-none transition-colors duration-150 focus-visible:outline-ring-inset data-disabled:cursor-not-allowed data-disabled:opacity-55 not-data-[mixed]:data-checked:border-transparent not-data-[mixed]:data-checked:bg-primary not-data-[mixed]:data-checked:[--focus-ring:var(--primary-foreground)]",
        size === "sm"
          ? "[--thumb-size:--spacing(4)] sm:[--thumb-size:--spacing(3)]"
          : "[--thumb-size:--spacing(5)] sm:[--thumb-size:--spacing(3)]",
        className,
      )}
      data-size={size}
      data-slot="switch"
      data-mixed={mixed ? "" : undefined}
      // Base UI overwrites aria-checked even with undefined; pass it only for mixed state.
      {...(mixed ? { "aria-checked": "mixed" as const } : {})}
      {...props}
    >
      <SwitchPrimitive.Thumb
        className={cn(
          "pointer-events-none block size-(--thumb-size) shrink-0 origin-left in-[[role=switch]:active,[data-slot=label]:active,[data-slot=field-label]:active]:not-data-disabled:scale-x-110 in-[[role=switch]:active,[data-slot=label]:active,[data-slot=field-label]:active]:rounded-[var(--thumb-size)/calc(var(--thumb-size)*1.1)] rounded-(--thumb-size) bg-white shadow-plate data-checked:bg-primary-foreground [transition:translate_.15s_var(--ease-out-quint),border-radius_.15s,scale_.1s_.1s,transform-origin_.15s] data-checked:origin-right data-checked:translate-x-(--thumb-size) motion-reduce:transition-none",
          mixed &&
            "translate-x-[calc(var(--thumb-size)/2)] opacity-70 data-checked:translate-x-[calc(var(--thumb-size)/2)]",
        )}
        data-slot="switch-thumb"
      />
    </SwitchPrimitive.Root>
  );
}

export { Switch };
