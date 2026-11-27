import { msg, useMessages } from "gt-react";

import { toastManager } from "./ui/toast";

// Resolved when shown, not when queued: the reset is found before the UI language has loaded.
function Message({ text }: { readonly text: string }) {
  return useMessages()(text);
}

export function showConnectionCatalogResetToast(backupPath: string): void {
  const id = toastManager.add({
    type: "warning",
    title: <Message text={msg("Saved connections were reset")} />,
    description: (
      <Message
        text={msg(
          "This system could no longer decrypt them, so add them again. A copy of the old file was kept beside the new one.",
        )}
      />
    ),
    timeout: 0,
    actionProps: {
      children: <Message text={msg("Copy path")} />,
      // The notice shows once, so it stays until the path is really on the clipboard.
      onClick: () => {
        void navigator.clipboard.writeText(backupPath).then(
          () => toastManager.close(id),
          () => undefined,
        );
      },
    },
  });
}
