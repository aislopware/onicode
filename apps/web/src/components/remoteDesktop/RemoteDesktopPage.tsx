import { useAtomValue } from "@effect/atom-react";
import { connectionStatusText } from "@onicode/client-runtime/connection";
import type {
  RemoteDesktopViewer,
  RemoteDesktopViewerStats,
} from "@onicode/client-runtime/remote-desktop/viewer";
import type { EnvironmentId } from "@onicode/contracts";
import { KeyboardIcon, MaximizeIcon, MonitorIcon } from "lucide-react";
import { useRef, useState } from "react";

import { useEnvironment } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import { remoteDesktopEnvironment } from "~/state/remoteDesktop";
import { useEnvironmentSessionState } from "~/state/session";
import { Button } from "../ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import { Menu, MenuItem, MenuPopup, MenuShortcut, MenuTrigger } from "../ui/menu";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { SidebarInset } from "../ui/sidebar";
import { Spinner } from "../ui/spinner";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  WorkspaceBreadcrumb,
  WorkspaceBreadcrumbItem,
  WorkspaceBreadcrumbSeparator,
  WorkspaceBreadcrumbText,
} from "../WorkspaceBreadcrumb";
import { WorkspacePageHeader } from "../WorkspacePageHeader";
import { formatRemoteDesktopStats, resolveRemoteDesktopPageState } from "./remoteDesktopPage.logic";
import { RemoteDesktopView } from "./RemoteDesktopView";

// Shortcuts the local OS acts on before this page sees them.
const CHORDS = [
  { label: "App switcher", keys: "⌘ Tab", codes: ["MetaLeft", "Tab"] },
  { label: "Spotlight", keys: "⌘ Space", codes: ["MetaLeft", "Space"] },
  { label: "Mission Control", keys: "⌃ ↑", codes: ["ControlLeft", "ArrowUp"] },
  { label: "Lock screen", keys: "⌃ ⌘ Q", codes: ["ControlLeft", "MetaLeft", "KeyQ"] },
] as const;

export function RemoteDesktopPage({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const environment = useEnvironment(environmentId);
  const session = useEnvironmentSessionState(environmentId);
  const permitted = useAtomValue(remoteDesktopEnvironment.connect.permissionAtom(environmentId));
  const canConnect = session.data === null && !session.hasError ? null : permitted;
  const subscribe =
    canConnect === true &&
    environment?.connection.phase === "connected" &&
    environment.serverConfig?.remoteDesktop === true;
  const query = useEnvironmentQuery(
    subscribe ? remoteDesktopEnvironment.state({ environmentId, input: {} }) : null,
  );
  const page = resolveRemoteDesktopPageState({
    environment,
    canConnect,
    state: query.data,
    error: query.error,
  });

  const [selectedDisplayId, setSelectedDisplayId] = useState<string | null>(null);
  const [stats, setStats] = useState<RemoteDesktopViewerStats | null>(null);
  const viewerRef = useRef<RemoteDesktopViewer | null>(null);
  const frameRef = useRef<HTMLDivElement | null>(null);
  const enterFullScreen = () => {
    void frameRef.current?.requestFullscreen().then(() => {
      // Escape then reaches the remote machine; holding it leaves full screen.
      const keyboard = (navigator as { keyboard?: { lock?: (keys: string[]) => Promise<void> } })
        .keyboard;
      void keyboard?.lock?.(["Escape"]).catch(() => undefined);
    });
  };

  const host = page.kind === "host" ? page.host : null;
  const displays = host?.displays ?? [];
  const display =
    displays.find((candidate) => candidate.id === selectedDisplayId) ??
    displays.find((candidate) => candidate.primary) ??
    displays[0] ??
    null;
  const label = environment?.label ?? "Remote desktop";

  return (
    <SidebarInset className="h-dvh min-h-0 overflow-hidden overscroll-y-none">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col bg-background">
        <WorkspacePageHeader className="border-b border-border">
          <WorkspaceBreadcrumb ariaLabel="Remote desktop breadcrumb" className="min-w-0 flex-1">
            <WorkspaceBreadcrumbItem>
              <h1>Remote desktop</h1>
            </WorkspaceBreadcrumbItem>
            <WorkspaceBreadcrumbSeparator />
            <WorkspaceBreadcrumbItem current className="min-w-10 shrink">
              <WorkspaceBreadcrumbText>{label}</WorkspaceBreadcrumbText>
            </WorkspaceBreadcrumbItem>
          </WorkspaceBreadcrumb>
          {host && display ? (
            <div className="flex shrink-0 items-center gap-1">
              {stats ? (
                <span className="hidden truncate text-xs text-muted-foreground tabular-nums md:block">
                  {formatRemoteDesktopStats(stats)}
                </span>
              ) : null}
              {displays.length > 1 ? (
                <Select
                  value={display.id}
                  onValueChange={(value) => {
                    if (value) setSelectedDisplayId(value);
                  }}
                >
                  <SelectTrigger
                    aria-label="Display"
                    size="compact"
                    variant="ghost"
                    className="w-auto min-w-0"
                  >
                    <SelectValue>{display.label}</SelectValue>
                  </SelectTrigger>
                  <SelectPopup align="end" alignItemWithTrigger={false}>
                    {displays.map((candidate) => (
                      <SelectItem key={candidate.id} value={candidate.id}>
                        {candidate.label}
                      </SelectItem>
                    ))}
                  </SelectPopup>
                </Select>
              ) : null}
              <Menu>
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <MenuTrigger
                        disabled={!host.control}
                        render={<Button size="icon-sm" variant="ghost" aria-label="Send keys" />}
                      />
                    }
                  >
                    <KeyboardIcon />
                  </TooltipTrigger>
                  <TooltipPopup side="bottom">Send keys</TooltipPopup>
                </Tooltip>
                <MenuPopup align="end">
                  {CHORDS.map((chord) => (
                    <MenuItem
                      key={chord.label}
                      onClick={() => viewerRef.current?.chord(chord.codes)}
                    >
                      {chord.label}
                      <MenuShortcut>{chord.keys}</MenuShortcut>
                    </MenuItem>
                  ))}
                </MenuPopup>
              </Menu>
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label="Full screen"
                      onClick={enterFullScreen}
                    />
                  }
                >
                  <MaximizeIcon />
                </TooltipTrigger>
                <TooltipPopup side="bottom">Full screen (hold Esc to leave)</TooltipPopup>
              </Tooltip>
            </div>
          ) : null}
        </WorkspacePageHeader>
        {host && display ? (
          <>
            {host.control ? null : (
              <p className="shrink-0 border-b border-border px-4 py-1.5 text-xs text-muted-foreground">
                View only. Input is off because that machine has not given Onicode Accessibility
                access, or its operating system is not supported for control yet.
              </p>
            )}
            <div ref={frameRef} className="min-h-0 flex-1">
              <RemoteDesktopView
                environmentId={environmentId}
                displayId={display.id}
                control={host.control}
                onStats={setStats}
                onViewer={(viewer) => {
                  viewerRef.current = viewer;
                }}
              />
            </div>
          </>
        ) : page.kind === "loading" ? (
          <div className="flex flex-1 items-center justify-center text-muted-foreground">
            <Spinner size="md" />
          </div>
        ) : (
          <Empty className="flex-1">
            <EmptyMedia variant="icon">
              <MonitorIcon />
            </EmptyMedia>
            <EmptyHeader>
              {page.kind === "unknown-environment" ? (
                <>
                  <EmptyTitle>Environment not found</EmptyTitle>
                  <EmptyDescription>
                    This device has no saved environment by that ID.
                  </EmptyDescription>
                </>
              ) : page.kind === "disconnected" ? (
                <>
                  <EmptyTitle>{label} is not connected</EmptyTitle>
                  <EmptyDescription>
                    {environment ? connectionStatusText(environment.connection) : null}
                  </EmptyDescription>
                </>
              ) : page.kind === "unsupported" ? (
                <>
                  <EmptyTitle>Update Onicode on {label}</EmptyTitle>
                  <EmptyDescription>
                    That machine runs a version of Onicode without remote desktop.
                  </EmptyDescription>
                </>
              ) : page.kind === "forbidden" ? (
                <>
                  <EmptyTitle>No access to this screen</EmptyTitle>
                  <EmptyDescription>
                    Seeing and controlling the screen needs the same access as that machine's
                    terminal. Its owner can grant it in Settings, under Sharing &amp; access.
                  </EmptyDescription>
                </>
              ) : page.kind === "error" ? (
                <>
                  <EmptyTitle>Could not load remote desktop</EmptyTitle>
                  <EmptyDescription>{page.message}</EmptyDescription>
                </>
              ) : page.kind === "no-host" ? (
                <>
                  <EmptyTitle>Nothing is sharing this screen</EmptyTitle>
                  <EmptyDescription>
                    Open Onicode on that machine and turn on Remote desktop in Settings.
                  </EmptyDescription>
                </>
              ) : (
                <>
                  <EmptyTitle>No display to share</EmptyTitle>
                  <EmptyDescription>That machine reports no connected display.</EmptyDescription>
                </>
              )}
            </EmptyHeader>
          </Empty>
        )}
      </div>
    </SidebarInset>
  );
}
