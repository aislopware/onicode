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

  /**
   * Sends the buffer and starts a fresh one, resolving to the sent replay's id. With `expectedId`,
   * sends only while that replay is still the current one.
   */
  const sendBuffered = (expectedId?: string): Promise<string | undefined> => {
    const run = tasks.then(async () => {
      const replayId = replay.getReplayId();
      // Stopped on a private address, or already sent for an earlier error. A stopped replay
      // would restart in full recording on flush, so it is left alone.
      if (replayId === undefined || (expectedId !== undefined && replayId !== expectedId)) return;
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
    tasks = run.catch(() => {});
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
