import {
  createRemoteDesktopViewer,
  type RemoteDesktopViewer,
  type RemoteDesktopViewerStats,
  type RemoteDesktopViewerStatus,
} from "@onicode/client-runtime/remote-desktop/viewer";
import type { EnvironmentId } from "@onicode/contracts";
import { useState } from "react";

import { useStableCallback } from "~/hooks/useStableCallback";
import { formatEnvironmentQueryError } from "~/state/query";
import { remoteDesktopEnvironment } from "~/state/remoteDesktop";
import { useAtomCommand } from "~/state/use-atom-command";
import { Button } from "../ui/button";
import { Spinner } from "../ui/spinner";

const captureKeyboard = (focused: boolean) => {
  void window.desktopBridge?.remoteDesktop.captureKeyboard(focused);
};

/** One display of a remote machine, streamed over WebRTC; the environment only relays the offer. */
export function RemoteDesktopView(props: {
  readonly environmentId: EnvironmentId;
  readonly displayId: string;
  readonly control: boolean;
  /** Null whenever the picture is not streaming. */
  readonly onStats?: (stats: RemoteDesktopViewerStats | null) => void;
  /** The running viewer, for sending key chords; null while none is mounted. */
  readonly onViewer?: (viewer: RemoteDesktopViewer | null) => void;
}) {
  const { environmentId, displayId, control } = props;
  const connect = useAtomCommand(remoteDesktopEnvironment.connect, { reportFailure: false });
  const [attempt, setAttempt] = useState(0);
  const [status, setStatus] = useState<{
    readonly status: RemoteDesktopViewerStatus;
    readonly detail: string | null;
  }>({ status: "connecting", detail: null });
  const signal = useStableCallback(async (offer: string) => {
    const result = await connect({ environmentId, input: { displayId, offer } });
    if (result._tag === "Failure") throw new Error(formatEnvironmentQueryError(result.cause));
    return result.value.answer;
  });
  const reportStats = useStableCallback((stats: RemoteDesktopViewerStats | null) =>
    props.onStats?.(stats),
  );
  const reportViewer = useStableCallback((viewer: RemoteDesktopViewer | null) =>
    props.onViewer?.(viewer),
  );
  const mount = (container: HTMLDivElement | null) => {
    if (!container) return;
    // The browser writes the clipboard only for a focused window, so a copy made on the host
    // while this one is in the background waits here.
    let unwritten: string | null = null;
    const write = (text: string) => {
      unwritten = null;
      void navigator.clipboard.writeText(text).catch(() => {
        unwritten = text;
      });
    };
    const viewer = createRemoteDesktopViewer({
      container,
      signal,
      control,
      onStatus: (next, detail) => {
        setStatus({ status: next, detail: detail ?? null });
        if (next !== "streaming") reportStats(null);
      },
      onKeyboardFocus: (focused) => {
        captureKeyboard(focused);
        if (!focused) return;
        if (unwritten !== null) return write(unwritten);
        // Whatever was copied here while the picture was out of focus is ready to paste there.
        void navigator.clipboard.readText().then(viewer.sendClipboard, () => undefined);
      },
      onStats: reportStats,
      onClipboard: write,
    });
    reportViewer(viewer);
    return () => {
      reportViewer(null);
      viewer.stop();
    };
  };

  // Keys typed into the picture belong to the remote machine, not this app's shortcuts.
  return (
    <div className="relative size-full bg-black" data-keybinding-capture={control ? "" : undefined}>
      <div
        key={`${environmentId}\u0000${displayId}\u0000${control}\u0000${attempt}`}
        ref={mount}
        className="absolute inset-0"
      />
      {status.status === "connecting" ? (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center gap-2 text-sm text-white/70">
          <Spinner size="sm" />
          Connecting…
        </div>
      ) : status.status === "reconnecting" ? (
        <div
          role="status"
          className="pointer-events-none absolute inset-0 flex items-center justify-center gap-2 bg-black/60 text-sm text-white/80"
        >
          <Spinner size="sm" />
          Reconnecting…
        </div>
      ) : status.status === "error" ? (
        <div
          role="alert"
          className="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-background px-6 text-center"
        >
          <p className="max-w-sm text-sm text-muted-foreground">
            {status.detail ?? "Could not connect to the host."}
          </p>
          <Button size="sm" variant="outline" onClick={() => setAttempt((value) => value + 1)}>
            Retry
          </Button>
        </div>
      ) : null}
    </div>
  );
}
