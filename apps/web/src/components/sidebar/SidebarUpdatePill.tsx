import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import { managedUpdatesDenial } from "@onicode/client-runtime/contract-labels";
import { type DesktopBridge, type DesktopUpdateState } from "@onicode/contracts";
import { TriangleAlertIcon } from "lucide-react";
import { msg, T, useGT, useMessages } from "gt-react";
import { useAtomValue } from "@effect/atom-react";
import * as Schema from "effect/Schema";
import { type ComponentProps, useEffect, useEffectEvent, useId, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { settlePromise, squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import { m } from "../../i18n/messages";
import { useLocalStorage } from "../../hooks/useLocalStorage";
import { useMediaQuery } from "../../hooks/useMediaQuery";
import { cn } from "../../lib/utils";
import { ensureLocalApi } from "../../localApi";
import { useDesktopUpdateState } from "../../state/desktopUpdate";
import { usePrimaryManagedPolicy } from "../../state/managedPolicy";
import { primaryServerSettingsAtom } from "../../state/server";
import { stackedThreadToast, toastManager } from "../ui/toast";
import {
  canCheckForUpdate,
  DESKTOP_UPDATE_PROMPT_SNOOZE_MS,
  getArm64IntelBuildWarningDescription,
  getDesktopUpdateActionError,
  getDesktopUpdateButtonTooltip,
  getDesktopUpdateDownloadedVersion,
  getDesktopUpdateIdleTooltip,
  getDesktopUpdateInstallConfirmationMessage,
  getDesktopUpdatePromptVersion,
  getDesktopUpdateRestartNote,
  getRequiredDesktopUpdateView,
  isDesktopUpdateButtonDisabled,
  resolveDesktopUpdateButtonAction,
  shouldShowArm64IntelBuildWarning,
  shouldToastDesktopUpdateActionResult,
} from "../desktopUpdate.logic";
import { showDesktopUpdatePrompt } from "../desktopUpdate.toast";
import { useCheckProviderUpdates } from "../ProviderUpdatesAction";
import { Alert, AlertDescription, AlertTitle } from "../ui/alert";
import { Popover, PopoverCreateHandle, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { SidebarMenuItem } from "../ui/sidebar";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  DesktopUpdateStatusIcon,
  type DesktopUpdateStatusIconState,
  shouldContinueDesktopUpdateCheckAnimation,
  shouldShowDesktopUpdateCheckIcon,
} from "./DesktopUpdateStatusIcon";
import { RequiredUpdateDialog } from "./RequiredUpdateDialog";
import { SidebarUpdateReleaseNotes } from "./SidebarUpdateReleaseNotes";
import { SidebarUpdateCapsule } from "./SidebarUpdateCapsule";
import { UpdateEverythingDialog } from "./UpdateEverythingDialog";
import type { UpdateEverythingRun, UpdateRunLocalState } from "./updateEverything.logic";
import {
  clearSavedUpdateEverythingResult,
  saveUpdateEverythingResult,
  useUpdateEverythingRun,
} from "./updateEverythingRun";
import { type ReportLocalUpdateError, useUpdateEverything } from "./useUpdateEverything";

type SidebarUpdatePopoverChangeDetails = Parameters<
  NonNullable<ComponentProps<typeof Popover>["onOpenChange"]>
>[1];
type SidebarUpdatePopoverHandle = ReturnType<typeof PopoverCreateHandle>;

function shouldUseSidebarUpdateReleaseNotesPopover(
  showUpdateDetails: boolean,
  state: DesktopUpdateState | null,
): boolean {
  return showUpdateDetails && (state?.releaseNotes.length ?? 0) > 0;
}

function handleSidebarUpdateReleaseNotesPopoverOpenChange(
  _open: boolean,
  details: Pick<SidebarUpdatePopoverChangeDetails, "reason" | "cancel">,
): void {
  if (details.reason === "trigger-press") details.cancel();
}

function openSidebarUpdateReleaseNotesPopoverOnForwardTab(
  event: { readonly key: string; readonly shiftKey: boolean },
  handle: Pick<SidebarUpdatePopoverHandle, "open">,
  triggerId: string,
): void {
  if (event.key !== "Tab" || event.shiftKey) return;
  // Hover-open popovers do not manage focus. Promote this one before native Tab runs.
  flushSync(() => handle.open(triggerId));
}

function resolveSidebarUpdatePresentation({
  action,
  showsEverything,
  run,
  isDownloading,
  showCheckIcon,
}: {
  readonly action: ReturnType<typeof resolveDesktopUpdateButtonAction>;
  readonly showsEverything: boolean;
  readonly run: UpdateEverythingRun | null;
  readonly isDownloading: boolean;
  readonly showCheckIcon: boolean;
}) {
  const showUpdateDetails = action !== "none" || showsEverything || run !== null || isDownloading;
  const iconStatus: DesktopUpdateStatusIconState = showCheckIcon
    ? "checking"
    : isDownloading
      ? "downloading"
      : run !== null
        ? run.localVersion !== null && action === "install"
          ? "downloaded"
          : "everything"
        : showsEverything
          ? "everything"
          : action === "install"
            ? "downloaded"
            : action === "download"
              ? "available"
              : "idle";

  return {
    iconStatus,
    showUpdateDetails,
    showUpdateIconState: showUpdateDetails && !showCheckIcon,
  } as const;
}

export function SidebarUpdateArchitectureWarning() {
  const state = useDesktopUpdateState();
  const messages = useMessages();
  const visible = shouldShowArm64IntelBuildWarning(state);
  const description =
    state && visible ? getArm64IntelBuildWarningDescription(state, messages) : null;

  if (!visible || !description) return null;

  return (
    <Alert variant="warning">
      <TriangleAlertIcon />
      <AlertTitle>
        <T>Intel build on Apple Silicon</T>
      </AlertTitle>
      <AlertDescription>{description}</AlertDescription>
    </Alert>
  );
}

const toastUpdateError: ReportLocalUpdateError = (title, description) => {
  toastManager.add(stackedThreadToast({ type: "error", title, description }));
};

// Long enough to read the dialog, short enough that an unattended machine updates the same minute.
const REQUIRED_UPDATE_RESTART_DELAY_MS = 60_000;
const PROMPT_SNOOZE_KEY = "onicode:desktop-update-prompt:snooze";
const PromptSnooze = Schema.NullOr(Schema.Struct({ version: Schema.String, until: Schema.Number }));

export async function downloadLocalUpdate(
  bridge: DesktopBridge,
  reportError: ReportLocalUpdateError = toastUpdateError,
): Promise<boolean> {
  try {
    const result = await bridge.downloadUpdate();
    const actionError = shouldToastDesktopUpdateActionResult(result)
      ? getDesktopUpdateActionError(result)
      : null;
    if (actionError) reportError(m(msg("Could not download update")), actionError);
    return result.completed;
  } catch (error) {
    reportError(
      m(msg("Could not start update download")),
      thrownErrorMessage(error, m) ?? m(msg("An unexpected error occurred.")),
    );
    return false;
  }
}

async function installLocalUpdate(
  bridge: DesktopBridge,
  reportError: ReportLocalUpdateError = toastUpdateError,
  expectedVersion?: string,
): Promise<boolean> {
  try {
    const result = await bridge.installUpdate(expectedVersion);
    if (!result.accepted) {
      reportError(
        m(msg("Could not install update")),
        m(msg("Onicode is busy with another update step. Try again in a moment.")),
      );
      return false;
    }
    const actionError = shouldToastDesktopUpdateActionResult(result)
      ? getDesktopUpdateActionError(result)
      : null;
    if (!actionError) return true;
    reportError(m(msg("Could not install update")), actionError);
    return false;
  } catch (error) {
    reportError(
      m(msg("Could not install update")),
      thrownErrorMessage(error, m) ?? m(msg("An unexpected error occurred.")),
    );
    return false;
  }
}

/** Installs outside an update-everything run; the relaunched app then confirms the new version. */
export async function installLocalUpdateWithNotice(
  bridge: DesktopBridge,
  version: string | null,
): Promise<boolean> {
  saveUpdateEverythingResult({
    localVersion: version,
    updatedServers: 0,
    updatedProviders: 0,
    failures: [],
    restarting: [],
  });
  const installed = await installLocalUpdate(bridge);
  if (!installed) clearSavedUpdateEverythingResult();
  return installed;
}

export function SidebarUpdatePill() {
  return <SidebarUpdateControl />;
}

function SidebarUpdateControl() {
  const state = useDesktopUpdateState();
  const gt = useGT();
  const messages = useMessages();
  // The desktop updates this machine, whose policy the primary environment reports.
  const updatesDenial = managedUpdatesDenial(usePrimaryManagedPolicy(), messages);
  const continuesThreads = useAtomValue(primaryServerSettingsAtom).continueThreadsAfterServerUpdate;
  const restartNote = getDesktopUpdateRestartNote(continuesThreads, messages);
  const [promptSnooze, setPromptSnooze] = useLocalStorage(PROMPT_SNOOZE_KEY, null, PromptSnooze);
  const [promptClock, setPromptClock] = useState(() => Date.now());
  const [requiredSkipped, setRequiredSkipped] = useState(false);
  const [isActionPending, setIsActionPending] = useState(false);
  const [providerCheck, setProviderCheck] = useState<"button" | "dialog" | null>(null);
  const isCheckingProviders = providerCheck === "button";
  const [isConfirmingEverything, setIsConfirmingEverything] = useState(false);
  const [checkAnimationKey, setCheckAnimationKey] = useState(0);
  const [releaseNotesPopoverHandle] = useState(() => PopoverCreateHandle());
  const suppressReleaseNotesFocusOpen = useRef(false);
  const releaseNotesPopupRef = useRef<HTMLDivElement>(null);
  const releaseNotesTriggerId = useId();
  const prefersReducedMotion = useMediaQuery("(prefers-reduced-motion: reduce)");

  const updateStatus = state?.status;
  const [isCheckAnimationLatched, setIsCheckAnimationLatched] = useState(
    () => !prefersReducedMotion && updateStatus === "checking",
  );
  const [latchInputs, setLatchInputs] = useState({ prefersReducedMotion, updateStatus });
  if (
    latchInputs.prefersReducedMotion !== prefersReducedMotion ||
    latchInputs.updateStatus !== updateStatus
  ) {
    setLatchInputs({ prefersReducedMotion, updateStatus });
    if (prefersReducedMotion) {
      setIsCheckAnimationLatched(false);
    } else if (updateStatus === "checking") {
      setIsCheckAnimationLatched(true);
    }
  }

  const action = state ? resolveDesktopUpdateButtonAction(state) : "none";
  const isDownloading = state?.status === "downloading";
  const localUpdateVersion =
    state && action !== "none" ? getDesktopUpdateDownloadedVersion(state) : null;
  const everything = useUpdateEverything(localUpdateVersion);
  const checkProviderUpdates = useCheckProviderUpdates();
  const runsEverything = everything.hasUpdates && !isDownloading;
  const run = useUpdateEverythingRun();
  const isRunningEverything = run !== null;
  const showCheckIcon = shouldShowDesktopUpdateCheckIcon({
    isAnimationLatched: isCheckAnimationLatched,
    isChecking: state?.status === "checking" || isCheckingProviders,
    prefersReducedMotion,
  });
  const { iconStatus, showUpdateDetails, showUpdateIconState } = resolveSidebarUpdatePresentation({
    action,
    showsEverything: runsEverything,
    run,
    isDownloading,
    showCheckIcon,
  });
  const idleTooltip =
    !showUpdateDetails && !showCheckIcon
      ? getDesktopUpdateIdleTooltip(state, updatesDenial, messages)
      : null;
  const tooltip = runsEverything
    ? everything.summary
    : showUpdateDetails
      ? state
        ? getDesktopUpdateButtonTooltip(state, messages)
        : gt("Update available")
      : showCheckIcon
        ? gt("Checking for updates…")
        : (idleTooltip?.title ?? gt("Check for updates"));
  const disabled = showCheckIcon
    ? true
    : runsEverything
      ? false
      : showUpdateDetails
        ? isDesktopUpdateButtonDisabled(state)
        : !canCheckForUpdate(state);
  const isInteractionDisabled = disabled || isActionPending || isRunningEverything;
  const showReleaseNotesPopover =
    !isRunningEverything && shouldUseSidebarUpdateReleaseNotesPopover(showUpdateDetails, state);

  const [releaseNotesOpen, setReleaseNotesOpen] = useState(false);
  const [releaseNotesTriggerFocusVisible, setReleaseNotesTriggerFocusVisible] = useState(false);
  // Release notes close when they stop applying, and open when they start
  // applying while the trigger already holds keyboard focus.
  const [releaseNotesShownBefore, setReleaseNotesShownBefore] = useState(showReleaseNotesPopover);
  if (releaseNotesShownBefore !== showReleaseNotesPopover) {
    setReleaseNotesShownBefore(showReleaseNotesPopover);
    setReleaseNotesOpen(showReleaseNotesPopover && releaseNotesTriggerFocusVisible);
  }

  const handleAction = async ({ skipConfirm = false }: { readonly skipConfirm?: boolean } = {}) => {
    const bridge = window.desktopBridge;
    if (isInteractionDisabled) return;

    if (runsEverything) {
      setIsConfirmingEverything(true);
      // Machines that still cache an older latest version add their rows while the dialog is open.
      setProviderCheck("dialog");
      void checkProviderUpdates({ quiet: true }).finally(() => setProviderCheck(null));
      return;
    }

    if (!bridge || !state) return;
    setIsActionPending(true);

    if (action === "download") {
      void downloadLocalUpdate(bridge).finally(() => setIsActionPending(false));
      return;
    }

    if (action === "install") {
      let confirmed = skipConfirm;
      try {
        if (!confirmed) {
          confirmed = await ensureLocalApi().dialogs.confirm(
            getDesktopUpdateInstallConfirmationMessage(state, continuesThreads),
            { initialFocus: "confirm" },
          );
        }
      } catch (error) {
        setIsActionPending(false);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: gt("Could not confirm update"),
            description: thrownErrorMessage(error, m) ?? gt("Update confirmation failed."),
          }),
        );
        return;
      }
      if (!confirmed) {
        setIsActionPending(false);
        return;
      }
      void installLocalUpdateWithNotice(bridge, localUpdateVersion).finally(() =>
        setIsActionPending(false),
      );
      return;
    }

    if (!prefersReducedMotion) {
      setIsCheckAnimationLatched(true);
      setCheckAnimationKey((key) => key + 1);
    }
    const appCheck = bridge
      .checkForUpdate()
      .then((result) => {
        if (result.checked) return;
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: gt("Could not check for updates"),
            description:
              updatesDenial ??
              result.state.message ??
              gt("Automatic updates are not available in this build."),
          }),
        );
      })
      .catch((error) => {
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: gt("Could not check for updates"),
            description: thrownErrorMessage(error, m) ?? gt("Update check failed."),
          }),
        );
      });
    setProviderCheck("button");
    const providersChecked = checkProviderUpdates().finally(() => setProviderCheck(null));
    void Promise.allSettled([appCheck, providersChecked]).then(() => setIsActionPending(false));
  };

  const runEverything = async () => {
    const bridge = window.desktopBridge;
    setIsConfirmingEverything(false);
    setIsActionPending(true);
    const result = await settlePromise(() =>
      everything.run({
        ...(bridge && action === "download"
          ? {
              downloadLocal: (reportError) => downloadLocalUpdate(bridge, reportError),
            }
          : {}),
        ...(bridge && action !== "none"
          ? { installLocal: (reportError) => installLocalUpdate(bridge, reportError) }
          : {}),
      }),
    );
    if (result._tag === "Failure") {
      const error = squashAtomCommandFailure(result);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: gt("Could not update everything"),
          description: thrownErrorMessage(error, m) ?? gt("An unexpected error occurred."),
        }),
      );
    }
    setIsActionPending(false);
  };

  const promptVersion =
    run === null && !isActionPending && !isConfirmingEverything
      ? getDesktopUpdatePromptVersion(state, promptSnooze, promptClock)
      : null;
  const promptTitle = gt("Onicode {version} is ready", { version: promptVersion ?? "" });
  const promptActionLabel = runsEverything
    ? gt("Update", { $context: "verb, confirms the update" })
    : gt("Restart to update");
  const runPromptAction = useEffectEvent(() => void handleAction({ skipConfirm: true }));
  const snoozePrompt = useEffectEvent((version: string) =>
    setPromptSnooze({ version, until: Date.now() + DESKTOP_UPDATE_PROMPT_SNOOZE_MS }),
  );
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- toasts live in a manager outside React; the prompt stays up while a downloaded update waits
  useEffect(() => {
    if (promptVersion === null) return;
    return showDesktopUpdatePrompt({
      shell: window.desktopBridge,
      version: promptVersion,
      title: promptTitle,
      note: restartNote,
      actionLabel: promptActionLabel,
      onAction: runPromptAction,
      onDismiss: () => snoozePrompt(promptVersion),
    });
  }, [promptVersion, promptTitle, restartNote, promptActionLabel]);
  const snoozedUntil = promptSnooze?.until ?? null;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- only a timer knows when the snooze runs out and the prompt is due again
  useEffect(() => {
    if (snoozedUntil === null) return;
    const timer = window.setTimeout(
      () => setPromptClock(Date.now()),
      Math.max(0, snoozedUntil - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [snoozedUntil]);

  const requiredUpdate = getRequiredDesktopUpdateView(state);
  const handleRequiredAction = async () => {
    const bridge = window.desktopBridge;
    if (!bridge || requiredUpdate === null || isActionPending) return;
    if (requiredUpdate.phase === "downloading") return;
    setIsActionPending(true);
    if (requiredUpdate.phase === "ready" || requiredUpdate.retry === "install") {
      await installLocalUpdateWithNotice(bridge, requiredUpdate.version);
    } else if (requiredUpdate.retry === "download") {
      await downloadLocalUpdate(bridge);
    } else {
      await bridge
        .checkForUpdate()
        .catch((error: unknown) =>
          toastUpdateError(
            gt("Could not check for updates"),
            thrownErrorMessage(error, m) ?? gt("Update check failed."),
          ),
        );
    }
    setIsActionPending(false);
  };
  // Restarting by itself is only safe when running threads pick up again afterwards.
  const restartsRequiredUpdate =
    requiredUpdate?.phase === "ready" && continuesThreads && !isActionPending;
  const requiredDialog = (
    <RequiredUpdateDialog
      view={requiredSkipped && requiredUpdate?.phase === "failed" ? null : requiredUpdate}
      note={restartNote}
      pending={isActionPending}
      restartDelayMs={restartsRequiredUpdate ? REQUIRED_UPDATE_RESTART_DELAY_MS : null}
      onAction={() => void handleRequiredAction()}
      onSkip={() => setRequiredSkipped(true)}
    />
  );

  const handleCheckAnimationIteration = () => {
    setIsCheckAnimationLatched(
      shouldContinueDesktopUpdateCheckAnimation({
        isChecking: state?.status === "checking" || isCheckingProviders,
        prefersReducedMotion,
      }),
    );
  };

  const statusIcon = (
    <DesktopUpdateStatusIcon
      key={showCheckIcon ? checkAnimationKey : iconStatus}
      downloadPercent={state?.downloadPercent ?? null}
      isCheckAnimating={showCheckIcon && !prefersReducedMotion}
      onCheckAnimationIteration={handleCheckAnimationIteration}
      status={iconStatus}
    />
  );
  const confirmDialog = (
    <UpdateEverythingDialog
      open={isConfirmingEverything}
      onOpenChange={setIsConfirmingEverything}
      version={everything.version}
      providerCount={everything.providerCount}
      rows={everything.rows}
      restarts={everything.restarts}
      checking={providerCheck === "dialog"}
      onConfirm={() => void runEverything()}
    />
  );

  if (run !== null) {
    const local: UpdateRunLocalState =
      run.localVersion === null
        ? null
        : state?.status === "downloading"
          ? { status: "downloading", percent: state.downloadPercent }
          : { status: "waiting" };
    return (
      <SidebarMenuItem className="@container/sidebar-update ml-auto flex min-w-8 flex-1">
        <SidebarUpdateCapsule
          run={run}
          local={local}
          icon={statusIcon}
          {...(run.localVersion === null && action === "install" && window.desktopBridge
            ? {
                onRestart: async () => {
                  await installLocalUpdateWithNotice(window.desktopBridge!, localUpdateVersion);
                },
              }
            : {})}
        />
        {confirmDialog}
        {requiredDialog}
      </SidebarMenuItem>
    );
  }

  const updateButton = (
    <button
      type="button"
      aria-label={
        idleTooltip?.detail
          ? gt("{tooltip}: {detail}", { tooltip, detail: idleTooltip.detail })
          : tooltip
      }
      aria-disabled={isInteractionDisabled || undefined}
      className={cn(
        "inline-flex size-8 items-center justify-center rounded-full outline-hidden ring-ring transition-colors focus-visible:ring-2 focus-visible:ring-inset",
        isInteractionDisabled ? "cursor-not-allowed" : "cursor-pointer",
        showUpdateIconState
          ? cn(
              "bg-sidebar-control-surface text-sidebar-foreground",
              !isInteractionDisabled && "hover:bg-sidebar-row-hover",
            )
          : cn(
              "text-(--sidebar-icon-color)",
              !isInteractionDisabled && "hover:bg-sidebar-row-hover hover:text-sidebar-foreground",
            ),
        disabled && !showUpdateIconState && "opacity-55",
      )}
      onClick={() => void handleAction()}
      onBlur={() => {
        suppressReleaseNotesFocusOpen.current = false;
        setReleaseNotesTriggerFocusVisible(false);
      }}
      onFocus={(event) => {
        const focusVisible = event.currentTarget.matches(":focus-visible");
        setReleaseNotesTriggerFocusVisible(focusVisible);
        if (!showReleaseNotesPopover || !focusVisible) return;
        if (suppressReleaseNotesFocusOpen.current) {
          suppressReleaseNotesFocusOpen.current = false;
          return;
        }
        flushSync(() => releaseNotesPopoverHandle.open(releaseNotesTriggerId));
      }}
      onKeyDown={(event) => {
        if (!showReleaseNotesPopover) return;
        openSidebarUpdateReleaseNotesPopoverOnForwardTab(
          event,
          releaseNotesPopoverHandle,
          releaseNotesTriggerId,
        );
      }}
    >
      {statusIcon}
    </button>
  );

  return (
    <SidebarMenuItem className="ml-auto shrink-0">
      <Popover
        handle={releaseNotesPopoverHandle}
        open={releaseNotesOpen && showReleaseNotesPopover}
        triggerId={releaseNotesTriggerId}
        onOpenChange={(open, details) => {
          if (open && !showReleaseNotesPopover) {
            details.cancel();
            return;
          }
          handleSidebarUpdateReleaseNotesPopoverOpenChange(open, details);
          if (!details.isCanceled) setReleaseNotesOpen(open);
        }}
      >
        <Tooltip disabled={showReleaseNotesPopover}>
          <TooltipTrigger
            id={releaseNotesTriggerId}
            render={
              <PopoverTrigger
                {...(!showReleaseNotesPopover
                  ? {
                      "aria-controls": undefined,
                      "aria-expanded": undefined,
                      "aria-haspopup": undefined,
                    }
                  : {})}
                closeDelay={150}
                handle={releaseNotesPopoverHandle}
                id={releaseNotesTriggerId}
                openOnHover={showReleaseNotesPopover}
                render={updateButton}
              />
            }
          />
          {!showReleaseNotesPopover ? (
            <TooltipPopup
              align="center"
              side="top"
              variant={showUpdateDetails ? "glass" : "default"}
            >
              {idleTooltip?.detail ? (
                <div className="text-left">
                  <div className="font-medium">{idleTooltip.title}</div>
                  <div className="text-muted-foreground">{idleTooltip.detail}</div>
                </div>
              ) : (
                <span className="whitespace-pre-line">{tooltip}</span>
              )}
            </TooltipPopup>
          ) : null}
        </Tooltip>
        {showReleaseNotesPopover && state ? (
          <PopoverPopup
            align="center"
            aria-label={gt("Update release notes")}
            initialFocus={false}
            onKeyDownCapture={(event) => {
              if (
                event.key === "Escape" &&
                releaseNotesPopupRef.current?.contains(document.activeElement)
              ) {
                suppressReleaseNotesFocusOpen.current = true;
              }
            }}
            ref={releaseNotesPopupRef}
            side="top"
            tooltipStyle
          >
            <SidebarUpdateReleaseNotes
              shell={window.desktopBridge}
              state={state}
              tooltip={tooltip}
            />
          </PopoverPopup>
        ) : null}
      </Popover>
      {confirmDialog}
      {requiredDialog}
    </SidebarMenuItem>
  );
}
