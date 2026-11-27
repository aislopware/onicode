import { useEffect, useRef, type RefObject } from "react";

import { isCommandPaletteOpen } from "../commandPaletteBus";
import {
  hasFloatingLayerOver,
  shouldRedirectInputToComposer,
} from "../components/chat/composerFocusRedirect";
import { isInSideChatPane, SIDE_CHAT_PANE_SELECTOR } from "../lib/sideChatPane";
import { getTerminalFocusOwner } from "../lib/terminalFocus";

function isFrozenComposerTarget(target: Element, form: HTMLFormElement | null) {
  return (
    !!form?.contains(target) &&
    !!target.closest('[data-testid="composer-editor"]') &&
    !target.closest("textarea, input, button")
  );
}

/** Keep submission shortcuts reachable when the frozen composer loses focus to the page. */
export function useDictationSubmissionShortcut(input: {
  enabled: boolean;
  /** A side chat's composer, which takes Enter only from focus inside its own pane. */
  background: boolean;
  formRef: RefObject<HTMLFormElement | null>;
  onKeyDown(event: KeyboardEvent): void;
}) {
  const latest = useRef(input);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- publishes committed inputs to the window keydown listener
  useEffect(() => {
    latest.current = input;
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to window keydown while dictation owns submission
  useEffect(() => {
    if (!input.enabled) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || isCommandPaletteOpen()) return;
      if (getTerminalFocusOwner() !== null) return;
      const target = event.target;
      if (!(target instanceof Element)) return;
      const { background, formRef } = latest.current;
      // Focus on the page itself belongs to the main chat; a side chat needs focus in its pane.
      const owned =
        target === document.body || target === document.documentElement
          ? !background && !hasFloatingLayerOver()
          : isInSideChatPane(target) === background &&
            (isFrozenComposerTarget(target, formRef.current) ||
              // Focus parked on the chat itself, such as the timeline, belongs to the composer.
              shouldRedirectInputToComposer(
                event,
                background ? target.closest(SIDE_CHAT_PANE_SELECTOR) : null,
              ));
      if (owned) latest.current.onKeyDown(event);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [input.enabled]);
}
