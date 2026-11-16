import { useAtomValue } from "@effect/atom-react";
import { useNavigate } from "@tanstack/react-router";
import { HardDriveIcon, MemoryStickIcon } from "lucide-react";

import { cn } from "../../lib/utils";
import { hostHardwareAlertAtom } from "../../state/presentation";
import { SidebarMenuButton, SidebarMenuItem, useSidebar } from "../ui/sidebar";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

/** In the sidebar's icon row only while a connected machine is running out of memory or storage. */
export function SidebarHardwareAlert() {
  const navigate = useNavigate();
  const { isMobile, setOpenMobile } = useSidebar();
  const alert = useAtomValue(hostHardwareAlertAtom);
  if (alert === null) return null;
  const Icon = alert.kind === "memory" ? MemoryStickIcon : HardDriveIcon;

  return (
    <SidebarMenuItem className="shrink-0">
      <Tooltip>
        <TooltipTrigger
          render={
            <SidebarMenuButton
              aria-label={alert.title}
              size="icon"
              onClick={() => {
                if (isMobile) setOpenMobile(false);
                void navigate({ to: "/settings/connections", hash: "environment-hardware" });
              }}
            >
              {/* Wrapped, because the button recolours an icon that is its direct child. */}
              <span
                className={cn(
                  "flex size-6 items-center justify-center rounded-md",
                  alert.level === "critical"
                    ? "bg-destructive/12 text-destructive"
                    : "bg-warning/12 text-warning",
                )}
              >
                <Icon className="size-4" />
              </span>
            </SidebarMenuButton>
          }
        />
        <TooltipPopup side="top">
          <span className="block font-medium">{alert.title}</span>
          {alert.description.split("\n").map((line) => (
            <span key={line} className="block">
              {line}
            </span>
          ))}
        </TooltipPopup>
      </Tooltip>
    </SidebarMenuItem>
  );
}
