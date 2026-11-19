import type { ScopedThreadRef } from "@onicode/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import { recordVisitForThread } from "~/browserHistoryStore";
import { readLocalApi } from "~/localApi";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { resolveExternalLinkUrl } from "./browserTargetResolver";
import {
  canOpenLinksInApp,
  resolveBrowserLinkTargetPreference,
  resolveLinkTarget,
} from "./browserLinkTarget";
import { isBrowserSettingsReadError, openUrlInPreview } from "./openFileInPreview";
import { msg } from "gt-react";
import { m } from "~/i18n/messages";

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
  return async (url, options = {}) => {
    const targetThreadRef = options.threadRef ?? threadRef;
    const target = resolveLinkTarget({
      url,
      event: options.event ?? NO_MODIFIER,
      preference: await resolveBrowserLinkTargetPreference(),
      canOpenInApp: canOpenLinksInApp(targetThreadRef),
    });
    // Null when the system browser would reach a different server on this computer.
    const externalUrl = targetThreadRef
      ? resolveExternalLinkUrl(targetThreadRef.environmentId, url)
      : url;
    if ((target === "app" || externalUrl === null) && targetThreadRef) {
      const result = await openUrlInPreview({ threadRef: targetThreadRef, url, openPreview });
      if (isAtomCommandInterrupted(result)) return;
      if (result._tag === "Success") {
        recordVisitForThread(targetThreadRef, url);
        return;
      }
      const failure = squashAtomCommandFailure(result);
      if (isBrowserSettingsReadError(failure)) throw failure;
      console.error(result.cause);
    }
    const api = readLocalApi();
    if (!api || externalUrl === null) throw new Error(m(msg("Link opening is unavailable.")));
    await api.shell.openExternal(externalUrl);
  };
}
