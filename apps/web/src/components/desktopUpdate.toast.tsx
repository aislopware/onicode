import type { DesktopBridge } from "@onicode/contracts";
import { msg, T } from "gt-react";
import { ArrowDownToLineIcon, ArrowRightIcon } from "lucide-react";

import { getDesktopUpdateReleaseUrl } from "./desktopUpdate.logic";
import { m } from "../i18n/messages";
import { toastManager } from "./ui/toast";

type DesktopUpdateShell = Pick<DesktopBridge, "openExternal">;

export async function openDesktopUpdateReleaseNotes(
  shell: DesktopUpdateShell | undefined,
  releaseUrl: string,
): Promise<void> {
  try {
    if (shell && (await shell.openExternal(releaseUrl))) return;
  } catch {}
  toastManager.add({ type: "error", title: m(msg("Unable to open release notes")) });
}

function ReleaseNotesLink({
  shell,
  releaseUrl,
}: {
  shell: DesktopUpdateShell;
  releaseUrl: string;
}) {
  return (
    <button
      className="ml-2 inline cursor-pointer text-muted-foreground underline decoration-dotted underline-offset-4 transition-colors hover:text-foreground"
      onClick={() => {
        void openDesktopUpdateReleaseNotes(shell, releaseUrl);
      }}
      type="button"
    >
      <T>What's new</T>
      <ArrowRightIcon
        aria-hidden
        className="ml-1 inline size-3 -rotate-45 align-[-0.125em]"
        strokeWidth={2.25}
      />
    </button>
  );
}

/** Shows the restart prompt until the returned function closes it; `onDismiss` runs when the user closes it instead. */
export function showDesktopUpdatePrompt({
  shell,
  version,
  title,
  note,
  actionLabel,
  onAction,
  onDismiss,
}: {
  readonly shell: DesktopUpdateShell | undefined;
  readonly version: string;
  readonly title: string;
  readonly note: string;
  readonly actionLabel: string;
  readonly onAction: () => void;
  readonly onDismiss: () => void;
}): () => void {
  const releaseUrl = getDesktopUpdateReleaseUrl(version);
  let dismissed = true;
  const id = toastManager.add({
    title,
    description: (
      <>
        {note}
        {shell && releaseUrl ? <ReleaseNotesLink releaseUrl={releaseUrl} shell={shell} /> : null}
      </>
    ),
    timeout: 0,
    onClose: () => {
      if (dismissed) onDismiss();
    },
    actionProps: { children: actionLabel, onClick: onAction },
    data: {
      actionLayout: "stacked-end",
      leadingIcon: <ArrowDownToLineIcon aria-hidden className="size-4 text-primary" />,
      secondaryActionProps: {
        children: m(msg("Later")),
        onClick: () => toastManager.close(id),
      },
      secondaryActionVariant: "ghost",
    },
  });
  return () => {
    dismissed = false;
    toastManager.close(id);
  };
}
