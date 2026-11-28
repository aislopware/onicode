import type { EnvironmentId, ScopedThreadRef } from "@onicode/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import { msg } from "gt-react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { recordVisitForThread } from "~/browserHistoryStore";
import { m } from "~/i18n/messages";
import { readLocalApi } from "~/localApi";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { resolveExternalLinkUrl } from "./browserTargetResolver";
import { canonicalLinkUrl } from "./webUrl";
import {
  canOpenLinksInApp,
  resolveBrowserLinkTargetPreference,
  resolveLinkTarget,
} from "./browserLinkTarget";
import { isBrowserSettingsReadError, openUrlInPreview } from "./openFileInPreview";
import {
  isLoopbackUnreachableError,
  LoopbackUnreachableError,
  useOpenInSystemBrowser,
} from "./useOpenInSystemBrowser";

const NO_MODIFIER = { metaKey: false, ctrlKey: false } as const;

/** Rejects on settings read failure without opening a browser, or if the system-browser fallback fails. */
export function useOpenLink(threadRef: ScopedThreadRef | null | undefined): (
  url: string,
  options?: {
    readonly event?: {
      readonly metaKey: boolean;
      readonly ctrlKey: boolean;
      readonly button?: number | undefined;
    };
    readonly threadRef?: ScopedThreadRef | undefined;
  },
) => Promise<void> {
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: false });
  const openInSystemBrowser = useOpenInSystemBrowser(threadRef?.environmentId ?? null);
  return async (link, options = {}) => {
    const url = canonicalLinkUrl(link);
    const targetThreadRef = options.threadRef ?? threadRef;
    const target = resolveLinkTarget({
      url,
      event: options.event ?? NO_MODIFIER,
      preference: await resolveBrowserLinkTargetPreference(),
      canOpenInApp: canOpenLinksInApp(targetThreadRef),
    });
    const openInApp = async (threadRef: ScopedThreadRef) => {
      const result = await openUrlInPreview({ threadRef, url, openPreview });
      if (isAtomCommandInterrupted(result)) return true;
      if (result._tag === "Success") {
        recordVisitForThread(threadRef, url);
        return true;
      }
      const failure = squashAtomCommandFailure(result);
      if (isBrowserSettingsReadError(failure)) throw failure;
      console.error(result.cause);
      return false;
    };
    if (target === "app" && targetThreadRef && (await openInApp(targetThreadRef))) return;
    try {
      // The forward belongs to this hook's environment; another one gets only a direct address.
      await (targetThreadRef && targetThreadRef.environmentId !== threadRef?.environmentId
        ? openAtDirectAddress(targetThreadRef.environmentId, url)
        : openInSystemBrowser(url));
    } catch (error) {
      // Without a forward or a direct address, the in-app preview still reaches the server.
      if (
        !isLoopbackUnreachableError(error) ||
        target === "app" ||
        !targetThreadRef ||
        !(await openInApp(targetThreadRef))
      ) {
        throw error;
      }
    }
  };
}

async function openAtDirectAddress(environmentId: EnvironmentId, url: string): Promise<void> {
  const externalUrl = resolveExternalLinkUrl(environmentId, url);
  if (externalUrl === null) throw new LoopbackUnreachableError({ message: url });
  const api = readLocalApi();
  if (!api) throw new Error(m(msg("Link opening is unavailable.")));
  await api.shell.openExternal(externalUrl);
}

/** Click handlers for an anchor that keeps its href for copying but opens as Settings → "Open links in" says. */
export function anchorOpenHandlers(open: ReturnType<typeof useOpenLink>, url: string) {
  const handle = (event: ReactMouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    void open(url, { event }).catch(console.error);
  };
  return {
    onClick: handle,
    onAuxClick: (event: ReactMouseEvent<HTMLAnchorElement>) => {
      if (event.button === 1) handle(event);
    },
  };
}
