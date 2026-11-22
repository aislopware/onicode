/** The part of Sentry's replay integration the hold drives. */
export interface HeldReplay {
  readonly getReplayId: () => string | undefined;
  readonly flush: (options: { readonly continueRecording: boolean }) => Promise<void>;
  readonly stop: (options: { readonly flush: boolean }) => Promise<void>;
  readonly startBuffering: () => void;
}

/**
 * Sends buffered replays one at a time and keeps the replay off private addresses. Sentry would
 * otherwise turn a sent buffer into a session that keeps uploading until the session ends.
 */
export function createReplayHold(replay: HeldReplay, addressIsPrivate: () => boolean) {
  let held = false;
  let interrupted = false;
  let tasks: Promise<unknown> = Promise.resolve();

  // In-flight sends by replay id, so a report opened during an error's send links that replay.
  const sends = new Map<string, Promise<string | undefined>>();

  /**
   * Sends the replay recording now, or `expectedId`, and starts a fresh buffer, resolving to the
   * sent replay's id. A replay already being sent is not sent twice.
   */
  const sendBuffered = (expectedId?: string): Promise<string | undefined> => {
    const replayId = expectedId ?? replay.getReplayId();
    // Stopped on a private address: nothing to send.
    if (replayId === undefined) return Promise.resolve(undefined);
    const sending = sends.get(replayId);
    if (sending) return sending;
    const run = tasks.then(async () => {
      // Already sent, or stopped since. A stopped replay would restart in full recording on
      // flush, so it is left alone.
      if (replay.getReplayId() !== replayId) return undefined;
      held = true;
      interrupted = false;
      try {
        await replay.flush({ continueRecording: false }).catch(() => {});
        return interrupted ? undefined : replayId;
      } finally {
        // Without a flush, stop drops the buffer synchronously, so nothing recorded since is sent.
        void replay.stop({ flush: false });
        held = false;
        if (!addressIsPrivate()) replay.startBuffering();
      }
    });
    sends.set(replayId, run);
    tasks = run.catch(() => {}).finally(() => sends.delete(replayId));
    return run;
  };

  /** Stops recording on a private address at once, even mid-send, and resumes off one. */
  const syncAddress = (): void => {
    const isPrivate = addressIsPrivate();
    if (isPrivate && replay.getReplayId() !== undefined) {
      void replay.stop({ flush: false });
      if (held) interrupted = true;
    } else if (!isPrivate && !held && replay.getReplayId() === undefined) {
      replay.startBuffering();
    }
  };

  return { sendBuffered, syncAddress };
}
