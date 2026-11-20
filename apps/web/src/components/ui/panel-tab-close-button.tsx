import { X } from "lucide-react";
import { Tooltip, TooltipPopup, TooltipTrigger } from "~/components/ui/tooltip";

interface PanelTabCloseButtonProps {
  label: string;
  onClick: () => void;
  tooltip?: string;
}

/** Sits over the end of a `relative group/tab`, hidden until the tab is hovered or holds focus.
 *  The tab's label takes `panel-tab-label` so its end fades under the button. */
export function PanelTabCloseButton({ label, onClick, tooltip }: PanelTabCloseButtonProps) {
  const button = (
    <button
      type="button"
      className="absolute top-1/2 right-1 flex size-4 -translate-y-1/2 cursor-pointer items-center justify-center rounded-sm after:absolute after:-inset-1 text-muted-foreground opacity-0 outline-none hover:bg-accent hover:text-foreground focus-visible:outline-ring-inset group-hover/tab:opacity-100 group-has-[:focus-visible]/tab:opacity-100 any-pointer-coarse:opacity-100"
      aria-label={label}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      <X className="size-3" />
    </button>
  );

  if (!tooltip) return button;

  return (
    <Tooltip>
      <TooltipTrigger render={button} />
      <TooltipPopup>{tooltip}</TooltipPopup>
    </Tooltip>
  );
}
