import { type ServerLifecycleWelcomePayload } from "@onicode/contracts";
import { scopedProjectKey, scopeProjectRef } from "@onicode/client-runtime/environment";
import {
  Outlet,
  Link,
  redirect,
  createRootRoute,
  type ErrorComponentProps,
  useLocation,
  useNavigate,
  useRouter,
} from "@tanstack/react-router";
import { msg, T, useGT, Var } from "gt-react";
import { BugIcon, CheckIcon, CopyIcon } from "lucide-react";
import { useEffect, useEffectEvent, useRef, useState } from "react";

import { APP_DISPLAY_NAME, APP_VERSION } from "../branding";
import { AppSidebarLayout } from "../components/AppSidebarLayout";
import { BugReportDialogHost } from "../components/BugReportDialog";
import { CommandPalette } from "../components/CommandPalette";
import { CustomSnoozeDialogHost } from "../components/CustomSnoozeDialog";
import { AddProjectToMachineDialogHost } from "../components/AddProjectToMachineDialog";
import { AgentSignInDialogHost } from "../components/mcp/AgentSignInDialogHost";
import { UseInAnotherAppDialogHost } from "../components/mcp/UseInAnotherAppDialog";
import { ConfirmDialogHost } from "../components/ConfirmDialogHost";
import { DevReplayHost } from "../components/DevReplayHost";
import { FirstRunGate } from "../components/onboarding/FirstRunGate";
import { SshPasswordPromptDialog } from "../components/desktop/SshPasswordPromptDialog";
import { SnapShotCoordinator } from "../components/desktop/SnapShotCoordinator";
import { DesktopAppActivationCoordinator } from "../components/desktop/DesktopAppActivationCoordinator";
import { DesktopWebLinkCoordinator } from "../components/desktop/DesktopWebLinkCoordinator";
import { RunningThreadKeepAlive } from "../components/desktop/RunningThreadKeepAlive";
import { RemoteDesktopHosting } from "../components/desktop/RemoteDesktopHosting";
import { ReopenClosedViewShortcut } from "../components/ReopenClosedViewShortcut";
import { ProjectCloneToastCoordinator } from "../components/ProjectCloneToastCoordinator";
import { ChatGptWelcomeCoordinator } from "../components/settings/ChatGptWelcomeCoordinator";
import { ThemeEditorHost } from "../components/settings/ThemeEditorHost";
import { useCopyToClipboard } from "../hooks/useCopyToClipboard";
import { PrimaryAuthErrorSurface } from "../components/auth/PrimaryAuthErrorSurface";
import { Button } from "../components/ui/button";
import { StandalonePage, StandalonePageHeader } from "../components/ui/standalone-page";
import { AnchoredToastProvider, ToastProvider } from "../components/ui/toast";
import { TooltipProvider } from "../components/ui/tooltip";
import { TruncationTooltip } from "../components/ui/truncation-tooltip";
import { useClientSettings } from "../hooks/useSettings";
import {
  deriveLogicalProjectKeyFromSettings,
  derivePhysicalProjectKeyFromPath,
  selectProjectGroupingSettings,
} from "@onicode/client-runtime/state/project-grouping";
import { useUiStateStore } from "../uiStateStore";
import { openBugReport, useBugReportStore } from "../bugReportStore";
import { bugReportsSupported, reportedErrorId } from "../observability/crashReportingStartup";
import { configureClientTracing } from "../observability/clientTracing";
import { resolveInitialServerAuthGateState } from "../environments/primary";
import { isLocalEnvironmentDisabled } from "../localEnvironment";
import { useAtomValue } from "@effect/atom-react";
import { primaryServerConfigAtom, primaryServerWelcomeAtom } from "../state/server";
import { readProject } from "../state/entities";

import { getDesktopSnapShotBridge } from "../lib/desktopSnapShot";
import { isChunkLoadError } from "../lib/chunkReloadGuard";
import { shouldResumeSnapShotSetupOnStartup } from "../lib/snapShotSetupResume";

export const Route = createRootRoute({
  beforeLoad: async ({ location }) => {
    // Desktop with its own environment switched off: only saved environments.
    if (isLocalEnvironmentDisabled()) {
      return {
        authGateState: {
          status: "hosted-static",
        } as const,
      };
    }

    const authGateState = await resolveInitialServerAuthGateState();
    if (authGateState.status === "authenticated") {
      // Idempotent, so every navigation can ask for it.
      void configureClientTracing();
    }
    if (
      authGateState.status === "authenticated" &&
      getDesktopSnapShotBridge() &&
      shouldResumeSnapShotSetupOnStartup() &&
      location.pathname !== "/settings/snap-shot"
    ) {
      throw redirect({ to: "/settings/snap-shot", replace: true });
    }
    return {
      authGateState,
    };
  },
  component: RootRouteView,
  errorComponent: RootRouteErrorView,
  notFoundComponent: RootRouteNotFoundView,
  head: () => ({
    meta: [{ name: "title", content: APP_DISPLAY_NAME }],
  }),
});

function RootRouteNotFoundView() {
  return (
    <main className="flex min-h-0 min-w-0 flex-1 items-center justify-center p-6">
      <div className="flex max-w-sm flex-col items-center gap-4 text-center">
        <h1 className="text-sm font-medium text-foreground">
          <T>Page not found</T>
        </h1>
        <p className="text-ui text-muted-foreground">
          <T>
            This link doesn't point to a page in <Var>{APP_DISPLAY_NAME}</Var>. Go home to choose a
            project or start a thread.
          </T>
        </p>
        <Button render={<Link to="/" replace />}>
          <T>Go home</T>
        </Button>
      </div>
    </main>
  );
}

function RootRouteView() {
  // Above every branch: app services raise toasts for the whole page, so the surface they write to
  // must not unmount when the root switches branches (for example /welcome to the app shell).
  return (
    <ToastProvider>
      <AnchoredToastProvider>
        {/* One group for the app: after the first tooltip, its neighbours open at once. */}
        <TooltipProvider>
          <RootRouteBranch />
        </TooltipProvider>
        <TruncationTooltip />
      </AnchoredToastProvider>
    </ToastProvider>
  );
}

function RootRouteBranch() {
  const pathname = useLocation({ select: (location) => location.pathname });
  const { authGateState } = Route.useRouteContext();
  const primaryEnvironmentAuthenticated = authGateState.status === "authenticated";
  // A sync IPC read; the setting only changes across a relaunch.
  const [localEnvironmentDisabled] = useState(isLocalEnvironmentDisabled);
  // Once true, stays true: a visit to /welcome means the bootstrap already chose a route.
  const [returningFromWelcome, setReturningFromWelcome] = useState(pathname === "/welcome");
  if (pathname === "/welcome" && !returningFromWelcome) setReturningFromWelcome(true);

  const isWelcome = pathname === "/welcome";
  if (authGateState.status === "requires-auth") {
    return <PrimaryAuthErrorSurface errorMessage={authGateState.errorMessage} />;
  }

  // Settings can reopen /welcome mid-session, so it mounts everything the app does except the
  // first-run gate and the event router, whose first navigation would leave the page.
  const appHosts = (
    <>
      {primaryEnvironmentAuthenticated || localEnvironmentDisabled ? (
        <>
          <DesktopAppActivationCoordinator />
          <DesktopWebLinkCoordinator />
        </>
      ) : null}
      <RunningThreadKeepAlive />
      {primaryEnvironmentAuthenticated ? <RemoteDesktopHosting /> : null}
      <SshPasswordPromptDialog />
      <SnapShotCoordinator />
      <ReopenClosedViewShortcut />
      <ConfirmDialogHost />
      <BugReportDialogHost />
      <CustomSnoozeDialogHost />
      <ProjectCloneToastCoordinator />
      <AddProjectToMachineDialogHost />
      <UseInAnotherAppDialogHost />
      <AgentSignInDialogHost />
      {import.meta.env.DEV && primaryEnvironmentAuthenticated ? <DevReplayHost /> : null}
    </>
  );
  const appShell = (
    <CommandPalette>
      {/* Setup starts with an empty workspace, so the Welcome page has no thread list. */}
      <AppSidebarLayout showSidebar={!isWelcome}>
        <Outlet />
      </AppSidebarLayout>
    </CommandPalette>
  );

  return (
    <>
      <ChatGptWelcomeCoordinator />
      {isWelcome ? (
        <>
          {appHosts}
          {appShell}
        </>
      ) : (
        <FirstRunGate
          enabled={primaryEnvironmentAuthenticated}
          hostedStatic={authGateState.status === "hosted-static"}
        >
          {appHosts}
          {primaryEnvironmentAuthenticated ? (
            <EventRouter skipInitialBootstrapNavigation={returningFromWelcome} />
          ) : null}
          {appShell}
        </FirstRunGate>
      )}
      {/* Above both shells and the router, so drafts survive navigation, including to /welcome. */}
      <ThemeEditorHost />
    </>
  );
}

function RootRouteErrorView({ error }: ErrorComponentProps) {
  const router = useRouter();
  const gt = useGT();
  const message = errorMessage(error) ?? gt("An unexpected router error occurred.");
  // Router pathname rather than window.location: desktop uses hash history, where the window path is always "/".
  const pathname = useLocation({ select: (location) => location.pathname });
  const report = errorReport(error, pathname);

  return (
    <StandalonePage>
      <StandalonePageHeader title={gt("Something went wrong.")} description={message} />

      <div className="mt-5 flex flex-wrap gap-2">
        <Button
          size="sm"
          onClick={() =>
            isChunkLoadError(error) ? window.location.reload() : void router.invalidate()
          }
        >
          <T>Try again</T>
        </Button>
        <Button size="sm" variant="outline" onClick={() => window.location.reload()}>
          <T>Reload app</T>
        </Button>
        <CopyErrorButton report={report} />
        <ReportErrorButton error={error} />
      </div>

      <div className="mt-5 overflow-hidden rounded-lg border border-border bg-background/55">
        <p className="px-3 py-1.5 text-xs font-medium text-muted-foreground">
          <T>Error report</T>
        </p>
        <pre className="max-h-64 overflow-auto border-t border-border bg-background/80 px-3 py-2 text-xs whitespace-pre-wrap text-foreground">
          {report}
        </pre>
      </div>
    </StandalonePage>
  );
}

function ReportErrorButton({ error }: { readonly error: unknown }) {
  const [session, setSession] = useState<number>();
  const sent = useBugReportStore(
    (state) => session !== undefined && state.sentSessions.includes(session),
  );
  if (!bugReportsSupported()) return null;
  return (
    <>
      <Button
        size="sm"
        variant="outline"
        disabled={sent}
        onClick={async () => {
          const opened = await openBugReport({ associatedEventId: reportedErrorId(error) });
          if (opened !== undefined) setSession(opened);
        }}
      >
        {sent ? <CheckIcon className="text-success" /> : <BugIcon />}
        {sent ? <T>Report sent</T> : <T>Report this bug</T>}
      </Button>
      {/* The error screen replaces the app shell and its dialog host. */}
      <BugReportDialogHost />
    </>
  );
}

function CopyErrorButton({ report }: { report: string }) {
  const { copyToClipboard, isCopied } = useCopyToClipboard({
    target: {
      target: "error-report",
      unavailableMessage: msg("Clipboard API is unavailable while copying error-report."),
      failureMessage: msg("Failed to copy error-report to the clipboard."),
    },
  });
  const gt = useGT();

  return (
    <Button size="sm" variant="outline" onClick={() => copyToClipboard(report)}>
      {isCopied ? <CheckIcon className="text-success" /> : <CopyIcon />}
      {isCopied ? gt("Copied") : gt("Copy error")}
    </Button>
  );
}

function errorMessage(error: unknown): string | null {
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message;
  }

  if (typeof error === "string" && error.trim().length > 0) {
    return error;
  }

  return null;
}

function errorDetails(error: unknown): string {
  if (error instanceof Error) {
    return error.stack ?? error.message;
  }

  if (typeof error === "string") {
    return error;
  }

  try {
    return JSON.stringify(error, null, 2);
  } catch {
    // oxlint-disable-next-line onicode/no-untranslated-copy -- error report pasted into bug reports
    return "No additional error details are available.";
  }
}

const MAX_ERROR_CAUSE_DEPTH = 5;

// Use only pathname to keep query tokens off the clipboard.
function errorReport(error: unknown, pathname: string): string {
  const lines = [
    `${APP_DISPLAY_NAME} ${APP_VERSION}`,
    `Path: ${pathname}`,
    `Time: ${new Date().toISOString()}`,
    "",
    errorDetails(error),
  ];
  let cause = error instanceof Error ? error.cause : undefined;
  for (let depth = 0; cause !== undefined && depth < MAX_ERROR_CAUSE_DEPTH; depth += 1) {
    lines.push("", "Caused by:", errorDetails(cause));
    cause = cause instanceof Error ? cause.cause : undefined;
  }
  return lines.join("\n");
}

function EventRouter({
  skipInitialBootstrapNavigation,
}: {
  readonly skipInitialBootstrapNavigation: boolean;
}) {
  const navigate = useNavigate();
  const pathname = useLocation({ select: (loc) => loc.pathname });
  const projectGroupingSettings = useClientSettings(selectProjectGroupingSettings);
  const serverConfig = useAtomValue(primaryServerConfigAtom);
  const serverWelcome = useAtomValue(primaryServerWelcomeAtom);
  const readPathname = useEffectEvent(() => pathname);
  const handledBootstrapThreadIdRef = useRef<string | null>(null);
  const skipInitialBootstrapNavigationRef = useRef(skipInitialBootstrapNavigation);

  const handleWelcome = useEffectEvent((payload: ServerLifecycleWelcomePayload | null) => {
    if (!payload) return;

    void (async () => {
      if (!payload.bootstrapProjectId || !payload.bootstrapThreadId) {
        return;
      }
      const bootstrapProject = readProject(
        scopeProjectRef(payload.environment.environmentId, payload.bootstrapProjectId),
      );
      const bootstrapProjectKey =
        (bootstrapProject
          ? deriveLogicalProjectKeyFromSettings(bootstrapProject, projectGroupingSettings)
          : null) ??
        (serverConfig?.cwd
          ? derivePhysicalProjectKeyFromPath(payload.environment.environmentId, serverConfig.cwd)
          : null) ??
        scopedProjectKey(
          scopeProjectRef(payload.environment.environmentId, payload.bootstrapProjectId),
        );
      useUiStateStore.getState().setProjectExpanded(bootstrapProjectKey, true);

      if (readPathname() !== "/") {
        return;
      }
      if (skipInitialBootstrapNavigationRef.current) {
        skipInitialBootstrapNavigationRef.current = false;
        handledBootstrapThreadIdRef.current = payload.bootstrapThreadId;
        return;
      }
      if (handledBootstrapThreadIdRef.current === payload.bootstrapThreadId) {
        return;
      }
      await navigate({
        to: "/$environmentId/$threadId",
        params: {
          environmentId: payload.environment.environmentId,
          threadId: payload.bootstrapThreadId,
        },
        replace: true,
      });
      handledBootstrapThreadIdRef.current = payload.bootstrapThreadId;
    })().catch(() => undefined);
  });

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- expands the bootstrap project in the UI store and navigates the router on a server welcome
  useEffect(() => {
    handleWelcome(serverWelcome);
  }, [serverWelcome]);

  return null;
}
