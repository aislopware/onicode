import { useAtomValue } from "@effect/atom-react";
import {
  hostHardwareAlertDescription,
  hostHardwareAlertTitle,
} from "@onicode/client-runtime/hostHardware";
import { useNavigate } from "@tanstack/react-router";
import { useLocale, useMessages } from "gt-react";
import { CpuIcon, HardDriveIcon, MemoryStickIcon } from "lucide-react";

import { cn } from "../../lib/utils";
import { hostHardwareAlertAtom } from "../../state/presentation";
import { SidebarMenuButton, SidebarMenuItem, useSidebar } from "../ui/sidebar";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

const ALERT_ICONS = { cpu: CpuIcon, memory: MemoryStickIcon, storage: HardDriveIcon };

/** In the sidebar's icon row only while a connected machine is short of memory, storage, or CPU. */
export function SidebarHardwareAlert() {
  const navigate = useNavigate();
  const { isMobile, setOpenMobile } = useSidebar();
  const alert = useAtomValue(hostHardwareAlertAtom);
  const messages = useMessages();
  const locale = useLocale();
  if (alert === null) return null;
  const Icon = ALERT_ICONS[alert.kind];
  const title = hostHardwareAlertTitle(alert, messages, locale);

  return (
    <SidebarMenuItem className="shrink-0">
      <Tooltip>
        <TooltipTrigger
          render={
            <SidebarMenuButton
              aria-label={title}
              size="icon"
              onClick={() => {
                if (isMobile) setOpenMobile(false);
                void navigate({ to: "/settings/connections", hash: "environment-hardware" });
              }}
            >
              {/* Wrapped, because the button recolours an icon that is its direct child. */}
              <span
                className={cn(
                  "flex size-6 items-center justify-center rounded-md transition-[opacity,scale] duration-200 ease-out-quint starting:opacity-0 motion-safe:starting:scale-95",
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
        <TooltipPopup
          side="top"
          description={
            <span className="whitespace-pre-line">
              {hostHardwareAlertDescription(alert, messages)}
            </span>
          }
        >
          {title}
        </TooltipPopup>
      </Tooltip>
    </SidebarMenuItem>
  );
}
