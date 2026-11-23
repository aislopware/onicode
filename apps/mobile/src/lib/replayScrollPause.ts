/** Movement past this many points is a drag, not a tap. */
const DRAG_SLOP = 10;
/** A fling keeps scrolling after the finger lifts; replay stays paused through its momentum. */
export const REPLAY_RESUME_DELAY_MS = 1_500;
/** A pause never outlives this without a lift, so a lost touch end cannot stop the replay for good. */
export const REPLAY_MAX_PAUSE_MS = 10_000;

export interface ReplayScrollPause {
  readonly touchStart: (x: number, y: number) => void;
  readonly touchMove: (x: number, y: number) => void;
  /** `touches` is how many fingers are still down. */
  readonly touchEnd: (touches: number) => void;
  /** A native gesture such as a scroll took the touch over; it ends every finger of the gesture. */
  readonly touchCancel: () => void;
  /** Resumes at once, so a capture that needs the replay running can read it. */
  readonly resumeNow: () => void;
}

/**
 * Pauses replay capture while the user drags or scrolls. Each capture renders the screen on the
 * main thread, which drops frames mid-scroll; the replay keeps the frames before and after.
 */
export function createReplayScrollPause(replay: {
  readonly pause: () => void;
  readonly resume: () => void;
}): ReplayScrollPause {
  let paused = false;
  let origin: { x: number; y: number } | undefined;
  let resumeTimer: ReturnType<typeof setTimeout> | undefined;

  const resumeNow = () => {
    if (resumeTimer !== undefined) clearTimeout(resumeTimer);
    resumeTimer = undefined;
    if (!paused) return;
    paused = false;
    replay.resume();
  };
  const resumeIn = (ms: number) => {
    if (resumeTimer !== undefined) clearTimeout(resumeTimer);
    resumeTimer = setTimeout(resumeNow, ms);
  };
  const pause = () => {
    if (!paused) {
      paused = true;
      replay.pause();
    }
    resumeIn(REPLAY_MAX_PAUSE_MS);
  };
  const lifted = () => {
    origin = undefined;
    if (paused) resumeIn(REPLAY_RESUME_DELAY_MS);
  };

  return {
    touchStart: (x, y) => {
      origin ??= { x, y };
      // A finger that stops a fling holds the pause until it lifts.
      if (paused) resumeIn(REPLAY_MAX_PAUSE_MS);
    },
    touchMove: (x, y) => {
      if (origin === undefined) return;
      if (Math.abs(x - origin.x) > DRAG_SLOP || Math.abs(y - origin.y) > DRAG_SLOP) {
        origin = undefined;
        pause();
      }
    },
    touchEnd: (touches) => {
      if (touches === 0) lifted();
    },
    touchCancel: () => {
      pause();
      lifted();
    },
    resumeNow,
  };
}
