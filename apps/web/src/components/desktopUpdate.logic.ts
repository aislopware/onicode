import type {
  DesktopBridge,
  DesktopUpdateActionResult,
  DesktopUpdateState,
} from "@onicode/contracts";
import { msg } from "gt-react";

import { m, type MessageResolver } from "../i18n/messages";

export type DesktopUpdateButtonAction = "download" | "install" | "none";

const DEFAULT_DESKTOP_RELEASE_REPOSITORY = "aislopware/onicode";
const GITHUB_REPOSITORY_SEGMENT = /^[A-Za-z0-9._-]+$/u;

function isValidRepositorySegment(value: string | undefined): value is string {
  return (
    value !== undefined && value !== "." && value !== ".." && GITHUB_REPOSITORY_SEGMENT.test(value)
  );
}

function desktopReleaseRepositoryUrl(repository: string | undefined): string {
  const configuredRepository = repository?.trim() ?? "";
  const [owner, name, ...extra] = configuredRepository.split("/");
  const releaseRepository =
    isValidRepositorySegment(owner) && isValidRepositorySegment(name) && extra.length === 0
      ? configuredRepository
      : DEFAULT_DESKTOP_RELEASE_REPOSITORY;
  return `https://github.com/${releaseRepository}/releases`;
}

/** The download RPC can beat the updater's downloadedVersion event; fall back to the requested version. */
export function getDesktopUpdateDownloadedVersion(state: DesktopUpdateState): string | null {
  return state.downloadedVersion ?? state.availableVersion;
}

export function getDesktopUpdateReleaseUrl(
  version: string | null,
  repository: string | undefined = import.meta.env.ONICODE_DESKTOP_UPDATE_REPOSITORY,
): string | null {
  const normalizedVersion = version?.trim();
  if (!normalizedVersion) return null;
  return `${desktopReleaseRepositoryUrl(repository)}/tag/v${encodeURIComponent(normalizedVersion)}`;
}

export function getDesktopUpdateReleaseHistoryUrl(
  repository: string | undefined = import.meta.env.ONICODE_DESKTOP_UPDATE_REPOSITORY,
): string {
  return desktopReleaseRepositoryUrl(repository);
}

export function resolveDesktopUpdateButtonAction(
  state: DesktopUpdateState,
): DesktopUpdateButtonAction {
  if (
    state.downloadedVersion &&
    (state.status === "downloaded" ||
      (state.status === "error" &&
        (state.errorContext === null || state.errorContext === "install")))
  ) {
    return "install";
  }
  if (state.status === "available") {
    return "download";
  }
  if (state.status === "error") {
    if (state.errorContext === "download" && state.availableVersion) {
      return "download";
    }
  }
  return "none";
}

export function shouldShowArm64IntelBuildWarning(state: DesktopUpdateState | null): boolean {
  return state?.hostArch === "arm64" && state.appArch === "x64";
}

export function isDesktopUpdateButtonDisabled(state: DesktopUpdateState | null): boolean {
  return state?.status === "downloading";
}

export function getArm64IntelBuildWarningDescription(
  state: DesktopUpdateState,
  resolve: MessageResolver = m,
): string {
  if (!shouldShowArm64IntelBuildWarning(state)) {
    return resolve(msg("This install is using the correct architecture."));
  }

  const action = resolveDesktopUpdateButtonAction(state);
  if (action === "download") {
    return resolve(
      msg(
        "This Mac has Apple Silicon, but Onicode is still running the Intel build under Rosetta. Download the available update to switch to the native Apple Silicon build.",
      ),
    );
  }
  if (action === "install") {
    return resolve(
      msg(
        "This Mac has Apple Silicon, but Onicode is still running the Intel build under Rosetta. Restart to install the downloaded Apple Silicon build.",
      ),
    );
  }
  return resolve(
    msg(
      "This Mac has Apple Silicon, but Onicode is still running the Intel build under Rosetta. The next app update will replace it with the native Apple Silicon build.",
    ),
  );
}

export function getDesktopUpdateButtonTooltip(
  state: DesktopUpdateState,
  resolve: MessageResolver = m,
): string {
  if (
    state.notAppliedVersion !== undefined &&
    (state.status === "available" || state.status === "downloaded") &&
    getDesktopUpdateDownloadedVersion(state) === state.notAppliedVersion
  ) {
    return resolve(
      msg("Update {version} did not apply. Click to retry.", { version: state.notAppliedVersion }),
    );
  }
  if (
    (state.status === "available" || state.status === "error") &&
    state.errorContext === "download" &&
    state.availableVersion
  ) {
    return resolve(
      msg("Download failed for {version}. Click to retry.", { version: state.availableVersion }),
    );
  }
  if (state.status === "available") {
    return state.availableVersion === null
      ? resolve(msg("Update available ready to download"))
      : resolve(msg("Update {version} ready to download", { version: state.availableVersion }));
  }
  if (state.status === "downloading") {
    return typeof state.downloadPercent === "number"
      ? resolve(
          msg("Downloading update ({percent}%)", { percent: Math.floor(state.downloadPercent) }),
        )
      : resolve(msg("Downloading update"));
  }
  if (state.status === "downloaded") {
    if (state.errorContext === "install" && state.downloadedVersion) {
      return resolve(
        msg("Install failed for {version}. Click to retry.", { version: state.downloadedVersion }),
      );
    }
    const version = state.downloadedVersion ?? state.availableVersion;
    return version === null
      ? resolve(msg("Update ready downloaded. Click to restart and install."))
      : resolve(msg("Update {version} downloaded. Click to restart and install.", { version }));
  }
  if (state.status === "error") {
    if (state.errorContext === "install" && state.downloadedVersion) {
      return resolve(
        msg("Install failed for {version}. Click to retry.", { version: state.downloadedVersion }),
      );
    }
    if (state.downloadedVersion) {
      return resolve(
        msg("Update {version} downloaded. Click to restart and install.", {
          version: state.downloadedVersion,
        }),
      );
    }
    return state.message ?? resolve(msg("Update failed"));
  }
  return resolve(msg("Up to date"));
}

export function getDesktopUpdateRestartNote(
  continuesThreads: boolean,
  resolve: MessageResolver = m,
): string {
  return continuesThreads
    ? resolve(
        msg(
          "Restarting takes a moment. Your threads and drafts are saved, and running threads continue on their own.",
        ),
      )
    : resolve(
        msg(
          "Restarting takes a moment. Your threads and drafts are saved, but running threads stop until you message them again.",
        ),
      );
}

export function getDesktopUpdateInstallConfirmationMessage(
  state: Pick<DesktopUpdateState, "availableVersion" | "downloadedVersion">,
  continuesThreads: boolean,
  resolve: MessageResolver = m,
): string {
  const version = state.downloadedVersion ?? state.availableVersion;
  const question = version
    ? resolve(msg("Restart Onicode to install update {version}?", { version }))
    : resolve(msg("Restart Onicode to install the update?"));
  return `${question}\n\n${getDesktopUpdateRestartNote(continuesThreads, resolve)}`;
}

export const DESKTOP_UPDATE_PROMPT_SNOOZE_MS = 4 * 60 * 60 * 1000;

export interface DesktopUpdatePromptSnooze {
  readonly version: string;
  readonly until: number;
}

/** The downloaded version the restart prompt offers, or null while the prompt stays away. */
export function getDesktopUpdatePromptVersion(
  state: DesktopUpdateState | null,
  snooze: DesktopUpdatePromptSnooze | null,
  now: number,
): string | null {
  if (!state || state.required === true) return null;
  const version = state.downloadedVersion;
  if (state.status !== "downloaded" || version === null || state.errorContext !== null) return null;
  // A restart already failed to apply this version; asking for another would repeat it.
  if (state.notAppliedVersion === version) return null;
  if (snooze !== null && snooze.version === version && now < snooze.until) return null;
  return version;
}

export type RequiredDesktopUpdateView =
  | {
      readonly phase: "downloading";
      readonly version: string | null;
      readonly percent: number | null;
    }
  | { readonly phase: "ready"; readonly version: string | null }
  | {
      readonly phase: "failed";
      readonly version: string | null;
      readonly retry: "check" | "download" | "install";
    };

/** What the blocking dialog shows for a required update, or null when none is required. */
export function getRequiredDesktopUpdateView(
  state: DesktopUpdateState | null,
): RequiredDesktopUpdateView | null {
  if (!state || state.required !== true) return null;
  const version = getDesktopUpdateDownloadedVersion(state);
  const action = resolveDesktopUpdateButtonAction(state);
  if (action === "install") {
    return state.errorContext === "install" || state.notAppliedVersion === version
      ? { phase: "failed", version, retry: "install" }
      : { phase: "ready", version };
  }
  if (state.status === "downloading") {
    return { phase: "downloading", version, percent: state.downloadPercent };
  }
  // A found update starts downloading on its own, and a check leads to one.
  if (
    state.status === "checking" ||
    (state.status === "available" && state.errorContext === null)
  ) {
    return { phase: "downloading", version, percent: null };
  }
  return { phase: "failed", version, retry: action === "download" ? "download" : "check" };
}

export function getDesktopUpdateActionError(result: DesktopUpdateActionResult): string | null {
  if (!result.accepted || result.completed) return null;
  if (typeof result.state.message !== "string") return null;
  const message = result.state.message.trim();
  return message.length > 0 ? message : null;
}

/**
 * macOS can refuse the restart after installUpdate resolves; the desktop then reports it as a
 * later state with errorContext "install". Success quits the app, so this never resolves then.
 */
export async function installDesktopUpdate(
  bridge: Pick<DesktopBridge, "installUpdate" | "onUpdateState">,
  expectedVersion?: string,
  onAccepted?: (result: DesktopUpdateActionResult) => void,
): Promise<DesktopUpdateActionResult> {
  let awaitingFailure = false;
  let reportFailure: (state: DesktopUpdateState) => void = () => {};
  const failure = new Promise<DesktopUpdateState>((resolve) => {
    reportFailure = resolve;
  });
  const unsubscribe = bridge.onUpdateState((state) => {
    if (awaitingFailure && state.errorContext === "install") reportFailure(state);
  });
  try {
    const result = await bridge.installUpdate(expectedVersion);
    if (!result.accepted || result.completed || getDesktopUpdateActionError(result) !== null) {
      return result;
    }
    awaitingFailure = true;
    onAccepted?.(result);
    return { ...result, state: await failure };
  } finally {
    unsubscribe();
  }
}

export function shouldToastDesktopUpdateActionResult(result: DesktopUpdateActionResult): boolean {
  return getDesktopUpdateActionError(result) !== null;
}

export function canCheckForUpdate(state: DesktopUpdateState | null): boolean {
  if (!state || !state.enabled) return false;
  return (
    state.status !== "checking" && state.status !== "downloading" && state.status !== "disabled"
  );
}

/** `managedDenial` is this machine's policy refusing updates (`managedUpdatesDenial`); it wins. */
export function getDesktopUpdateDisabledReason(
  state: Pick<DesktopUpdateState, "enabled" | "status" | "message"> | null,
  managedDenial?: string,
  resolve: MessageResolver = m,
): string | null {
  if (managedDenial !== undefined) return managedDenial;
  if (!state || (state.enabled && state.status !== "disabled")) return null;
  return (
    state.message?.trim() || resolve(msg("Automatic updates are not available in this build."))
  );
}

export function getDesktopUpdateIdleTooltip(
  state: Pick<DesktopUpdateState, "enabled" | "status" | "message"> | null,
  managedDenial?: string,
  resolve: MessageResolver = m,
) {
  const disabledReason = getDesktopUpdateDisabledReason(state, managedDenial, resolve);
  return disabledReason
    ? { title: resolve(msg("Updates unavailable")), detail: disabledReason }
    : { title: resolve(msg("Check for updates")), detail: null };
}
