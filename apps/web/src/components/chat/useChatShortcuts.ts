import type { KeybindingCommand, ResolvedKeybindingsConfig } from "@onicode/contracts";
import { useEffect, useEffectEvent } from "react";
import { isCommandPaletteOpen } from "../../commandPaletteBus";
import { resolveChatShortcutCommand, type ShortcutMatchContext } from "../../keybindings";
import { isTerminalCloseConfirmPending } from "../../lib/terminalCloseConfirm";
import {
  preventRepeatedTerminalCloseShortcut,
  preventTerminalCloseShortcut,
} from "../../lib/terminalCloseShortcut";
import { isInSideChatPane } from "../../lib/sideChatPane";
import { getTerminalFocusOwner, type TerminalFocusOwner } from "../../lib/terminalFocus";
import type { ComposerHandleRef } from "../../composerHandleContext";
import { registerShortcutContext } from "../../shortcuts/dispatcher";
import type { BranchToolbarHandle } from "../BranchToolbar";
import { shouldTypeToFocusComposer } from "./composerFocusRedirect";
import { composerShortcutCommands } from "./composerShortcuts";
import { isImeKeyDown } from "../../lib/imeKeyDown";

/** Returning false lets the key through, except for cycling while a terminal owns focus. */
type ChatShortcutHandler = (context: {
  readonly event: KeyboardEvent;
  readonly terminalFocusOwner: TerminalFocusOwner | null;
}) => boolean | void;

export type ChatShortcutCommands = Partial<Record<KeybindingCommand, ChatShortcutHandler>>;

/** Window-level commands the main chat still runs while focus is in its side chat pane. */
const SIDE_CHAT_PANE_COMMANDS: ReadonlySet<KeybindingCommand> = new Set([
  "rightPanel.toggle",
  "rightPanel.close",
  "rightPanel.toggleMaximized",
]);

interface ChatShortcutsInput {
  readonly enabled: boolean;
  /**
   * A side chat inside another chat leaves the window to the outer chat. It listens on its
   * side chat pane, around `composerElement`: find and Escape closing it anywhere in the pane,
   * composer chords only inside `composerElement`.
   */
  readonly embedded?: boolean;
  readonly composerElement?: HTMLElement | null;
  readonly keybindings: ResolvedKeybindingsConfig;
  readonly scriptKeybindings: ResolvedKeybindingsConfig;
  readonly getShortcutContext: (target: EventTarget | null) => ShortcutMatchContext & {
    readonly modelPickerOpen: boolean;
  };
  readonly threadFindActive: boolean;
  /** Takes type-to-focus keys and the composer and branch toolbar chords. */
  readonly composerRef: ComposerHandleRef;
  readonly branchToolbarRef: { readonly current: BranchToolbarHandle | null };
  readonly closeThreadFind: () => void;
  readonly commands: ChatShortcutCommands;
  /** Handles commands not in `commands`, such as project scripts. */
  readonly fallback: (command: KeybindingCommand) => ChatShortcutHandler | undefined;
}

/**
 * The chat's window keyboard shortcuts, plus Escape closing thread find.
 * Listeners attach once; the effect events read the latest input, so handlers
 * act on the current render without re-registering on every change.
 */
export function useChatShortcuts(input: ChatShortcutsInput) {
  const embedded = input.embedded === true;
  const composerElement = embedded ? (input.composerElement ?? null) : null;
  const handleShortcut = useEffectEvent((event: KeyboardEvent) => {
    const { keybindings } = input;
    if (preventRepeatedTerminalCloseShortcut(event, keybindings)) {
      event.stopPropagation();
      return;
    }
    if (input.threadFindActive && event.key === "Escape") return;
    if (isTerminalCloseConfirmPending() && preventTerminalCloseShortcut(event, keybindings)) {
      event.stopPropagation();
      return;
    }
    if (!input.enabled || isCommandPaletteOpen()) return;
    const inSideChatPane = isInSideChatPane(event.target);
    const terminalFocusOwner = getTerminalFocusOwner();
    if (event.defaultPrevented && terminalFocusOwner === null) return;
    const shortcutContext = input.getShortcutContext(event.target);

    if (
      !inSideChatPane &&
      !shortcutContext.terminalFocus &&
      !shortcutContext.modelPickerOpen &&
      shouldTypeToFocusComposer(event) &&
      input.composerRef.current?.insertTextAtEnd(event.key) === true
    ) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }

    const command = resolveChatShortcutCommand(event, keybindings, input.scriptKeybindings, {
      context: shortcutContext,
    });
    if (!command || (inSideChatPane && !SIDE_CHAT_PANE_COMMANDS.has(command))) return;
    const run =
      input.commands[command] ??
      composerShortcutCommands(input.composerRef, input.branchToolbarRef)[command] ??
      input.fallback(command);
    if (!run) return;
    const handled = run({ event, terminalFocusOwner });
    if (
      handled === false &&
      (terminalFocusOwner === null ||
        (command !== "terminal.previous" && command !== "terminal.next"))
    )
      return;
    event.preventDefault();
    event.stopPropagation();
  });
  const dismissFind = useEffectEvent((event: KeyboardEvent) => {
    if (
      event.key !== "Escape" ||
      !input.threadFindActive ||
      event.defaultPrevented ||
      isImeKeyDown(event) ||
      isCommandPaletteOpen() ||
      isInSideChatPane(event.target) !== embedded
    )
      return;
    const context = input.getShortcutContext(event.target);
    if (context.terminalFocus || context.previewFocus || context.modelPickerOpen) return;
    // The narrow right panel sheet closes on Escape and ignores defaultPrevented.
    event.preventDefault();
    event.stopPropagation();
    input.closeThreadFind();
  });
  const handleSideChatShortcut = useEffectEvent((event: KeyboardEvent) => {
    if (event.defaultPrevented || isCommandPaletteOpen()) return;
    const command = resolveChatShortcutCommand(event, input.keybindings, input.scriptKeybindings, {
      context: input.getShortcutContext(event.target),
    });
    let run: ChatShortcutHandler | undefined;
    if (command === "chat.find") run = input.commands[command];
    else if (command && event.target instanceof Node && composerElement?.contains(event.target)) {
      run = composerShortcutCommands(input.composerRef, input.branchToolbarRef)[command];
    }
    if (!run || run({ event, terminalFocusOwner: null }) === false) return;
    event.preventDefault();
    event.stopPropagation();
  });
  // A side chat pane's composer is not this chat's, so its keys get none of this chat's context.
  const readShortcutContext = useEffectEvent((event: KeyboardEvent) =>
    isInSideChatPane(event.target) ? {} : input.getShortcutContext(event.target),
  );
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- lends this chat's when-clause context (composerEmpty, turnRunning) to the app's global shortcut scopes
  useEffect(() => {
    if (embedded || !input.enabled) return;
    return registerShortcutContext((event) => readShortcutContext(event));
  }, [embedded, input.enabled]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- attaches keydown listeners for chat shortcuts and find dismissal: on the window, or on an embedded chat's side chat pane
  useEffect(() => {
    const target: Pick<HTMLElement, "addEventListener" | "removeEventListener"> | null = embedded
      ? (composerElement?.closest<HTMLElement>("[data-side-chat-pane]") ?? null)
      : window;
    if (!target) return;
    const onKeyDownCapture = (event: KeyboardEvent) =>
      embedded ? handleSideChatShortcut(event) : handleShortcut(event);
    const onKeyDown = (event: KeyboardEvent) => dismissFind(event);
    target.addEventListener("keydown", onKeyDownCapture, true);
    target.addEventListener("keydown", onKeyDown);
    return () => {
      target.removeEventListener("keydown", onKeyDownCapture, true);
      target.removeEventListener("keydown", onKeyDown);
    };
  }, [composerElement, embedded]);
}
