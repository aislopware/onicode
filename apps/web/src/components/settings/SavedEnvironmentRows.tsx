import { SessionPermissions } from "./SessionPermissions";
import {
  ChevronRightIcon,
  ChevronsLeftRightEllipsisIcon,
  EllipsisIcon,
  MonitorIcon,
  RouteIcon,
} from "lucide-react";
import { useAtomValue } from "@effect/atom-react";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { type EnvironmentId, resolveEnvironmentMachineKind } from "@onicode/contracts";
import {
  connectionRoutes,
  hasSavedBearerRoute,
  connectionErrorText,
  connectionStatusText,
  environmentMcpUrl,
  localEnvironmentMcpUrl,
  type ConnectionCatalogEntry,
} from "@onicode/client-runtime/connection";
import { msg, T, useGT, useMessages } from "gt-react";
import { m, type MessageResolver } from "../../i18n/messages";

import { useCopyToClipboard } from "../../hooks/useCopyToClipboard";
import { cn } from "../../lib/utils";
import { EnvironmentIconMenu } from "./EnvironmentIconPicker";
import { EnvironmentRoutesList } from "./EnvironmentRoutesList";
import {
  ConnectionDiagnosisPanel,
  ConnectionDiagnosisSummary,
  useConnectionDiagnosis,
} from "./ConnectionDiagnosisDetails";
import { usePreparedConnection } from "~/state/session";
import { EnvironmentRow, environmentTransportLabel } from "./EnvironmentRow";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger } from "../ui/menu";
import { Switch } from "../ui/switch";
import { stackedThreadToast, toastManager } from "../ui/toast";
import { SharedTooltipTrigger } from "../ui/shared-tooltip";
import { Button } from "../ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "../ui/empty";
import {
  resolveServerConfigVersionMismatch,
  resolveServerSelfUpdateCapability,
  supportsDesktopAppUpdate,
} from "~/versionSkew";
import { environmentCatalog } from "~/connection/catalog";
import { isLocalLoopbackHost } from "@onicode/shared/hostClassification";
import { type EnvironmentPresentation, usePrimaryEnvironmentId } from "~/state/environments";
import { usePublicAccess } from "./publicAccess";
import { SettingsRow } from "./settingsLayout";
import { useSearchableSetting } from "./settingsSearch";
import { APP_VERSION } from "~/branding";
import { useAtomCommand } from "../../state/use-atom-command";
import { serverEnvironment } from "~/state/server";
import { canOpenRemoteDesktop } from "../remoteDesktop/remoteDesktopPage.logic";
import {
  OutdatedServerUpdateAction,
  ServerUpdateAction,
  ServerUpdateProgress,
} from "../ServerUpdateAction";

type SavedBackendListRowProps = {
  environment: EnvironmentPresentation;
  removingEnvironmentId: EnvironmentId | null;
  onSetEnabled: (environmentId: EnvironmentId, enabled: boolean) => void;
  onRemove: (environment: EnvironmentPresentation) => void;
  onAddRoute: (environment: EnvironmentPresentation) => void;
  /** The map's details open on a machine's routes; the list keeps them folded. */
  initialRoutesOpen?: boolean;
};

export function savedBackendStatus(
  environment: EnvironmentPresentation,
  resolve: MessageResolver = m,
): {
  readonly text: string;
  readonly tone: "muted" | "error";
} {
  if (!environment.entry.enabled && environment.connection.phase !== "unsupported")
    return { text: resolve(msg("Off", { $context: "environment switched off" })), tone: "muted" };
  const { connection } = environment;
  const error = connectionErrorText(connection, resolve);
  switch (connection.phase) {
    case "connected":
      return { text: resolve(msg("Connected")), tone: "muted" };
    case "connecting":
      return { text: resolve(msg("Connecting")), tone: "muted" };
    case "reconnecting":
      return {
        text: error
          ? resolve(msg("Reconnecting: {error}", { error }))
          : resolve(msg("Reconnecting")),
        tone: "error",
      };
    case "unsupported":
      return { text: resolve(msg("Client not supported")), tone: "muted" };
    case "error":
      return {
        text: error
          ? resolve(msg("Connection failed: {error}", { error }))
          : resolve(msg("Connection failed")),
        tone: "error",
      };
    case "offline":
      return { text: resolve(msg("Offline")), tone: "muted" };
    case "available":
      return { text: resolve(msg("Not connected")), tone: "muted" };
  }
}

function useCopyMcpUrl() {
  const gt = useGT();
  return useCopyToClipboard<{ url: string }>({
    target: {
      target: "MCP URL",
      unavailableMessage: msg("Clipboard API is unavailable while copying MCP URL."),
      failureMessage: msg("Failed to copy MCP URL to the clipboard."),
    },
    onCopy: ({ url }) => {
      toastManager.add({
        type: "success",
        title: gt("MCP URL copied"),
        description: gt("Add it to an agent, e.g. claude mcp add --transport http onicode {url}", {
          url,
        }),
      });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: gt("Could not copy MCP URL"),
          description: error.message,
        }),
      );
    },
  });
}

export function SavedBackendListRow({
  environment,
  removingEnvironmentId,
  onSetEnabled,
  onRemove,
  onAddRoute,
  initialRoutesOpen = false,
}: SavedBackendListRowProps) {
  const messages = useMessages();
  const gt = useGT();
  const [routesOpen, setRoutesOpen] = useState(initialRoutesOpen);
  const [permissionsOpen, setPermissionsOpen] = useState(false);
  const [diagnosisOpen, setDiagnosisOpen] = useState(false);
  const environmentId = environment.environmentId;
  const unsupported = environment.connection.phase === "unsupported";
  const blocked = unsupported && !hasSavedBearerRoute(environment.entry);
  const enabled = environment.entry.enabled && !unsupported;
  const isConnected = environment.connection.phase === "connected";
  const isRemoving = removingEnvironmentId === environmentId;
  const errorTraceId = environment.connection.traceId;
  const { copyToClipboard: copyTraceIdToClipboard } = useCopyToClipboard<{ traceId: string }>({
    target: {
      target: "trace ID",
      unavailableMessage: msg("Clipboard API is unavailable while copying trace ID."),
      failureMessage: msg("Failed to copy trace ID to the clipboard."),
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: gt("Could not copy trace ID"),
          description: error.message,
        }),
      );
    },
  });
  const copyTraceId = (traceId: string) => {
    copyTraceIdToClipboard(traceId, { traceId });
  };
  const { copyToClipboard: copyMcpUrl } = useCopyMcpUrl();
  const versionMismatch = resolveServerConfigVersionMismatch(environment.serverConfig, messages);
  const serverUpdateState = useAtomValue(serverEnvironment.updateStateAtom(environmentId));
  const resumingServerUpdate =
    serverUpdateState.status === "running" && serverUpdateState.stage === "resuming";
  const status = savedBackendStatus(environment, messages);
  const serverVersion = environment.serverConfig?.environment.serverVersion ?? null;
  const retryNow = useAtomCommand(environmentCatalog.retryNow);
  const { diagnosis: currentDiagnosis, state: connectionState } =
    useConnectionDiagnosis(environment);
  const diagnosis = resumingServerUpdate ? null : currentDiagnosis;
  const prepared = usePreparedConnection(environmentId);
  const connectedTarget = isConnected && prepared._tag === "Some" ? prepared.value.target : null;
  const mcpUrl = environmentMcpUrl({
    entry: environment.entry,
    connectedTarget,
  });
  const machineKind = resolveEnvironmentMachineKind(environment.serverConfig);
  const routeCount = connectionRoutes(environment.entry).length;
  const navigate = useNavigate();
  const remoteDesktopAvailable = canOpenRemoteDesktop(
    environment,
    usePrimaryEnvironmentId(),
    import.meta.env.DEV,
  );
  const subtitleText = [
    environmentTransportLabel(environment, connectedTarget, messages),
    // The headline below names the failure in full.
    diagnosis ? null : resumingServerUpdate ? gt("Restarting") : status.text,
    enabled && versionMismatch ? serverVersion : null,
  ]
    .filter((value): value is string => value !== null)
    .join(" · ");

  const showUpdateAction =
    enabled &&
    isConnected &&
    versionMismatch !== null &&
    (serverUpdateState.status === "idle" || serverUpdateState.status === "failed");

  const statusTooltip = unsupported
    ? (connectionErrorText(environment.connection, messages) ??
      connectionStatusText(environment.connection, messages))
    : enabled
      ? connectionStatusText(environment.connection, messages)
      : gt("Switched off");
  const statusUpdateTooltip = versionMismatch
    ? gt("Update available: {serverVersion} → {clientVersion}", {
        serverVersion: versionMismatch.serverVersion,
        clientVersion: versionMismatch.clientVersion,
      })
    : undefined;

  return (
    <EnvironmentRow
      kind={machineKind}
      label={environment.label}
      dimmed={!enabled}
      subtitle={
        <span className="flex min-w-0 items-center gap-1">
          <SharedTooltipTrigger
            render={
              <span
                className={cn(
                  "min-w-0 truncate",
                  enabled &&
                    status.tone === "error" &&
                    !resumingServerUpdate &&
                    !diagnosis &&
                    "text-destructive",
                )}
              />
            }
            label={statusTooltip}
            description={statusUpdateTooltip}
          >
            {subtitleText}
          </SharedTooltipTrigger>
          <span aria-hidden className="shrink-0">
            ·
          </span>
          <button
            type="button"
            aria-expanded={routesOpen}
            onClick={() => setRoutesOpen((open) => !open)}
            className="inline-flex shrink-0 items-center gap-0.5 rounded-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          >
            {routeCount === 1
              ? gt("Routes")
              : gt("{count, plural, one {{count} route} other {{count} routes}}", {
                  count: routeCount,
                })}
            <ChevronRightIcon
              aria-hidden
              className={cn(
                "size-3 shrink-0 transition-transform duration-150 motion-reduce:transition-none",
                routesOpen && "rotate-90",
              )}
            />
          </button>
          <span aria-hidden className="shrink-0">
            ·
          </span>
          <button
            type="button"
            aria-expanded={permissionsOpen}
            aria-controls={`remote-permissions-${environmentId}`}
            onClick={() => setPermissionsOpen((open) => !open)}
            className="inline-flex shrink-0 items-center gap-0.5 rounded-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          >
            <T>Permissions</T>
            <ChevronRightIcon
              aria-hidden
              className={cn(
                "size-3 shrink-0 transition-transform duration-150 motion-reduce:transition-none",
                permissionsOpen && "rotate-90",
              )}
            />
          </button>
        </span>
      }
      below={
        <>
          {diagnosis ? (
            <ConnectionDiagnosisSummary
              diagnosis={diagnosis}
              retryAt={connectionState.phase === "backoff" ? connectionState.retryAt : null}
              attempt={connectionState.attempt}
              expanded={diagnosisOpen}
              onToggle={() => setDiagnosisOpen((open) => !open)}
            />
          ) : null}
          {serverUpdateState.status !== "idle" ? (
            <div className="mt-1 max-w-md">
              <ServerUpdateProgress state={serverUpdateState} />
            </div>
          ) : null}
        </>
      }
      detail={
        <>
          {diagnosis && diagnosisOpen ? (
            <ConnectionDiagnosisPanel
              hostLabel={environment.label}
              diagnosis={diagnosis}
              onRetry={() => void retryNow(environmentId)}
            />
          ) : null}
          {routesOpen && (
            <EnvironmentRoutesList
              environment={environment}
              onAddRoute={() => onAddRoute(environment)}
            />
          )}
          {permissionsOpen && (
            <div id={`remote-permissions-${environmentId}`} className="mt-2 border-t border-border">
              <SessionPermissions
                environmentId={environmentId}
                connected={isConnected}
                routeContext
              />
            </div>
          )}
        </>
      }
    >
      {unsupported &&
      environment.entry.serverUpdateRequired === true &&
      serverUpdateState.status !== "running" ? (
        <OutdatedServerUpdateAction
          environmentId={environmentId}
          serverLabel={gt("{label} server", {
            label: environment.label,
            $context: "names a Onicode server inside sentences such as {serverLabel} updated",
          })}
          fromVersion={serverVersion ?? undefined}
          targetVersion={APP_VERSION}
          label={serverUpdateState.status === "failed" ? gt("Retry update") : gt("Update")}
        />
      ) : null}
      {showUpdateAction ? (
        <ServerUpdateAction
          environmentId={environmentId}
          serverLabel={gt("{label} server", {
            label: environment.label,
            $context: "names a Onicode server inside sentences such as {serverLabel} updated",
          })}
          selfUpdate={resolveServerSelfUpdateCapability(environment.serverConfig)}
          installation={environment.serverConfig?.environment.capabilities.serverInstallation}
          desktopAppUpdate={supportsDesktopAppUpdate(environment.serverConfig)}
          targetVersion={versionMismatch.clientVersion}
          label={serverUpdateState.status === "failed" ? gt("Retry update") : gt("Update")}
          appearance="icon"
        />
      ) : null}
      {remoteDesktopAvailable ? (
        <SharedTooltipTrigger
          render={
            <Button
              type="button"
              variant="ghost-muted"
              size="icon-xs"
              aria-label={`Open remote desktop for ${environment.label}`}
              onClick={() =>
                void navigate({
                  to: "/remote-desktop/$environmentId",
                  params: { environmentId },
                })
              }
            />
          }
          label="Open remote desktop"
        >
          <MonitorIcon className="size-3.5" />
        </SharedTooltipTrigger>
      ) : null}
      <SharedTooltipTrigger
        render={
          <Switch
            size="sm"
            checked={enabled}
            disabled={isRemoving && !blocked}
            readOnly={blocked}
            aria-label={
              enabled
                ? gt("Switch off {label}", { label: environment.label })
                : gt("Switch on {label}", { label: environment.label })
            }
            onCheckedChange={(checked) => onSetEnabled(environmentId, checked)}
          />
        }
        label={blocked ? gt("Client not supported") : enabled ? gt("Switch off") : gt("Switch on")}
      />
      <Menu>
        <MenuTrigger
          render={
            <Button
              type="button"
              variant="ghost-muted"
              size="icon-xs"
              disabled={isRemoving}
              aria-label={gt("More actions for {label}", { label: environment.label })}
            />
          }
        >
          <EllipsisIcon className="size-3.5" />
        </MenuTrigger>
        <MenuPopup align="end">
          <EnvironmentIconMenu
            environmentId={environmentId}
            serverConfig={environment.serverConfig}
          />
          <MenuItem onClick={() => setRoutesOpen((open) => !open)}>
            <RouteIcon />
            {routesOpen ? gt("Hide routes") : gt("Routes")}
          </MenuItem>
          {mcpUrl ? (
            <MenuItem onClick={() => copyMcpUrl(mcpUrl, { url: mcpUrl })}>
              <T>Copy MCP URL</T>
            </MenuItem>
          ) : null}
          {errorTraceId ? (
            <MenuItem onClick={() => copyTraceId(errorTraceId)}>
              <T>Copy trace ID</T>
            </MenuItem>
          ) : null}
          <MenuSeparator />
          <MenuItem variant="destructive" onClick={() => onRemove(environment)}>
            {isRemoving ? gt("Removing…") : gt("Remove from this device…")}
          </MenuItem>
        </MenuPopup>
      </Menu>
    </EnvironmentRow>
  );
}

/** Copying the host's own MCP address, for hosted agents or agents on this machine. */
export function LocalEnvironmentMcpUrlRow({
  environmentId,
  entry,
  publicAccess,
}: {
  readonly environmentId: EnvironmentId;
  readonly entry: ConnectionCatalogEntry;
  /** Whether the server has public access; older servers have no status to subscribe to. */
  readonly publicAccess: boolean;
}) {
  return publicAccess ? (
    <PublicAccessMcpUrlRow environmentId={environmentId} entry={entry} />
  ) : (
    <McpUrlRow entry={entry} publicMcpUrl={null} />
  );
}

function PublicAccessMcpUrlRow({
  environmentId,
  entry,
}: {
  readonly environmentId: EnvironmentId;
  readonly entry: ConnectionCatalogEntry;
}) {
  // Public access through Funnel is the address hosted agents can reach.
  const publicMcpUrl = usePublicAccess(environmentId).mcpUrl;
  return <McpUrlRow entry={entry} publicMcpUrl={publicMcpUrl} />;
}

function McpUrlRow({
  entry,
  publicMcpUrl,
}: {
  readonly entry: ConnectionCatalogEntry;
  readonly publicMcpUrl: string | null;
}) {
  const setting = useSearchableSetting();
  const gt = useGT();
  const { copyToClipboard: copyMcpUrl } = useCopyMcpUrl();
  const mcpUrl = publicMcpUrl ?? localEnvironmentMcpUrl({ entry });
  if (mcpUrl === null) return null;
  const loopback = isLocalLoopbackHost(new URL(mcpUrl).hostname);
  return (
    <SettingsRow
      {...setting("mcp-url")}
      description={
        publicMcpUrl !== null
          ? gt("Give this URL to a hosted agent, such as ChatGPT: {url}", { url: mcpUrl })
          : loopback
            ? gt("Give this URL to an agent running on this machine: {url}", { url: mcpUrl })
            : gt("Give this URL to an agent that can reach this environment: {url}", {
                url: mcpUrl,
              })
      }
      control={
        <Button size="sm" variant="outline" onClick={() => copyMcpUrl(mcpUrl, { url: mcpUrl })}>
          <T>Copy</T>
        </Button>
      }
    />
  );
}

export function EmptyRemoteEnvironments() {
  return (
    <Empty className="min-h-52">
      <EmptyMedia variant="icon">
        <ChevronsLeftRightEllipsisIcon />
      </EmptyMedia>
      <EmptyHeader>
        <EmptyTitle>
          <T>No saved remote environments</T>
        </EmptyTitle>
        <EmptyDescription>
          <T>Click “Add environment” to pair another environment.</T>
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}
