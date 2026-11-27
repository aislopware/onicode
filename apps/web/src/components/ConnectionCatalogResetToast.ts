import { msg } from "gt-react";

import { m } from "../i18n/messages";
import { toastManager } from "./ui/toast";

export function showConnectionCatalogResetToast(backupPath: string): void {
  const id = toastManager.add({
    type: "warning",
    title: m(msg("Saved connections were reset")),
    description: m(
      msg(
        "This system could no longer decrypt them, so add them again. A copy of the old file was kept beside the new one.",
      ),
    ),
    timeout: 0,
    actionProps: {
      children: m(msg("Copy path")),
      onClick: () => {
        toastManager.close(id);
        void navigator.clipboard.writeText(backupPath).catch(() => undefined);
      },
    },
  });
}
