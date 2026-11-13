import { Atom } from "effect/reactivity";

// How long a window in front counts as watched after its last input.
const PRESENCE_WINDOW_MS = 60_000;

// Tracked from the first import rather than from the first reader, so input that led to a thread
// counts. Until there is some, a window that came up by itself has nobody known to be at it.
let lastInputAt = Number.NEGATIVE_INFINITY;
const watchers = new Set<() => void>();
const notify = () => {
  for (const watcher of watchers) watcher();
};
const onInput = () => {
  const wasIdle = Date.now() - lastInputAt >= PRESENCE_WINDOW_MS;
  lastInputAt = Date.now();
  // Pointer moves arrive by the hundred, and only the first after a pause changes anything.
  if (wasIdle) notify();
};
// Bringing the window to the front is someone's doing too.
const onFocus = () => {
  lastInputAt = Date.now();
  notify();
};
if (typeof window !== "undefined") {
  // Capture: the terminal keeps the keys it handles from bubbling this far.
  const early = { capture: true, passive: true } as const;
  window.addEventListener("focus", onFocus);
  window.addEventListener("blur", notify);
  document.addEventListener("visibilitychange", notify);
  window.addEventListener("pointermove", onInput, early);
  window.addEventListener("pointerdown", onInput, early);
  window.addEventListener("keydown", onInput, early);
  window.addEventListener("wheel", onInput, early);
}

/** Input this window's own listeners never see, such as in a preview's page. */
export const noteUserInput = onInput;

const isPresent = () =>
  document.visibilityState === "visible" &&
  document.hasFocus() &&
  Date.now() - lastInputAt < PRESENCE_WINDOW_MS;

/**
 * Whether someone is at this window: it is in front and had input lately. A machine left running
 * as a host keeps its window in front for days with nobody there.
 */
export const userPresentAtom = Atom.make((get) => {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const read = () => {
    clearTimeout(timer);
    const present = isPresent();
    // Nothing fires when input stops, so the end of the window needs its own wake-up.
    if (present) timer = setTimeout(update, lastInputAt + PRESENCE_WINDOW_MS - Date.now());
    return present;
  };
  const update = () => get.setSelf(read());
  watchers.add(update);
  get.addFinalizer(() => {
    watchers.delete(update);
    clearTimeout(timer);
  });
  return read();
}).pipe(Atom.withLabel("user-present"));
