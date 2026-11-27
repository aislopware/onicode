import { T, useGT } from "gt-react";
import { useEffect, useEffectEvent, useState } from "react";

import { useNowSecond } from "../../hooks/useNowSecond";
import type { RequiredDesktopUpdateView } from "../desktopUpdate.logic";
import {
  AlertDialog,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../ui/alert-dialog";
import { Button } from "../ui/button";

interface RequiredUpdateDialogProps {
  /** Null keeps the dialog closed. */
  readonly view: RequiredDesktopUpdateView | null;
  readonly note: string;
  readonly pending: boolean;
  /** Runs `onAction` by itself this long after the update is ready; null waits for a click. */
  readonly restartDelayMs: number | null;
  readonly onAction: () => void;
  /** Leaves the app usable after a failure, so a broken update cannot lock anyone out. */
  readonly onSkip: () => void;
}

export function RequiredUpdateDialog({
  view,
  note,
  pending,
  restartDelayMs,
  onAction,
  onSkip,
}: RequiredUpdateDialogProps) {
  // No onOpenChange: Escape and outside presses leave a required update on screen.
  return (
    <AlertDialog open={view !== null}>
      <AlertDialogPopup className="sm:max-w-md">
        {view === null ? null : (
          <RequiredUpdateDialogContent
            view={view}
            note={note}
            pending={pending}
            restartDelayMs={restartDelayMs}
            onAction={onAction}
            onSkip={onSkip}
          />
        )}
      </AlertDialogPopup>
    </AlertDialog>
  );
}

function RestartCountdown({
  delayMs,
  onElapsed,
}: {
  readonly delayMs: number;
  readonly onElapsed: () => void;
}) {
  const gt = useGT();
  const [restartAt] = useState(() => Date.now() + delayMs);
  const elapse = useEffectEvent(onElapsed);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- only a timer can restart an unattended machine once its required update is ready
  useEffect(() => {
    const timer = window.setTimeout(elapse, delayMs);
    return () => window.clearTimeout(timer);
  }, [delayMs]);
  const seconds = Math.max(0, Math.ceil((restartAt - useNowSecond()) / 1000));
  return gt("Restarting in {seconds}s", { seconds });
}

function RequiredUpdateDialogContent({
  view,
  note,
  pending,
  restartDelayMs,
  onAction,
  onSkip,
}: RequiredUpdateDialogProps & { readonly view: RequiredDesktopUpdateView }) {
  const gt = useGT();
  const { version } = view;
  const status =
    view.phase === "downloading" ? (
      typeof view.percent === "number" ? (
        gt("Downloading… {percent}%", { percent: Math.floor(view.percent) })
      ) : (
        gt("Downloading…")
      )
    ) : view.phase === "failed" ? (
      view.retry === "install" ? (
        gt("The update did not install.")
      ) : (
        gt("The update could not be downloaded.")
      )
    ) : restartDelayMs === null ? null : (
      <RestartCountdown delayMs={restartDelayMs} onElapsed={onAction} />
    );

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>
          <T>Update required</T>
        </AlertDialogTitle>
        <AlertDialogDescription>
          {version === null
            ? gt(
                "This version of Onicode is no longer supported. Install the update to keep working.",
              )
            : gt(
                "This version of Onicode is no longer supported. Install {version} to keep working.",
                { version },
              )}{" "}
          {note}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <AlertDialogFooter>
        {status === null ? null : (
          <p
            role="status"
            className="self-center text-muted-foreground text-sm max-sm:order-1 max-sm:text-center sm:flex-1"
          >
            {status}
          </p>
        )}
        {view.phase === "failed" ? (
          <Button variant="outline" onClick={onSkip}>
            <T>Not now</T>
          </Button>
        ) : null}
        <Button disabled={pending || view.phase === "downloading"} onClick={onAction}>
          {view.phase === "failed"
            ? gt("Try again")
            : restartDelayMs === null
              ? gt("Restart to update")
              : gt("Restart now")}
        </Button>
      </AlertDialogFooter>
    </>
  );
}
