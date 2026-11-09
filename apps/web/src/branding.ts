import type { DesktopAppBranding } from "@onicode/contracts";

function readInjectedDesktopAppBranding(): DesktopAppBranding | null {
  if (typeof window === "undefined") {
    return null;
  }

  return window.desktopBridge?.getAppBranding() ?? null;
}

const injectedDesktopAppBranding = readInjectedDesktopAppBranding();
export const APP_BASE_NAME = injectedDesktopAppBranding?.baseName ?? "Onicode";
export const APP_STAGE_LABEL: "Dev" | null = injectedDesktopAppBranding
  ? injectedDesktopAppBranding.stageLabel
  : import.meta.env.DEV
    ? "Dev"
    : null;
export const APP_DISPLAY_NAME =
  injectedDesktopAppBranding?.displayName ??
  (APP_STAGE_LABEL === null ? APP_BASE_NAME : `${APP_BASE_NAME} (${APP_STAGE_LABEL})`);
export const APP_VERSION = import.meta.env.APP_VERSION || "0.0.0";
