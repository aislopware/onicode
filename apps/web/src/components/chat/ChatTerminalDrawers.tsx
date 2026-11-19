import type { ResolvedKeybindingsConfig } from "@onicode/contracts";
import {
  parseScopedThreadKey,
  scopedThreadKey,
  scopeThreadRef,
} from "@onicode/client-runtime/environment";
import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useComposerDraftStore } from "../../composerDraftStore";
import type { TerminalContextSelection } from "../../lib/terminalContext";
import { usePanelAnimationSettings, usePanelPresence } from "../../panelAnimations";
import { useRightPanelStore } from "../../rightPanelStore";
import { useThreadRefs } from "../../state/entities";
import { useTerminalUiStateStore } from "../../terminalUiStateStore";
import {
  MAX_HIDDEN_MOUNTED_TERMINAL_THREADS,
  reconcileMountedTerminalThreadIds,
} from "../ChatView.logic";
import {
  PersistentThreadTerminalDrawer,
  ThreadTerminalReconciler,
  type PersistentTerminalLaunchContext,
} from "../PersistentThreadTerminals";

interface ChatTerminalDrawersProps {
  readonly activeThreadKey: string | null;
  readonly activeTerminalOpen: boolean;
  readonly composerHeight: number;
  readonly launchContext: PersistentTerminalLaunchContext | null;
  readonly splitShortcutLabel: string | undefined;
  readonly splitVerticalShortcutLabel: string | undefined;
  readonly newShortcutLabel: string | undefined;
  readonly closeShortcutLabel: string | undefined;
  readonly keybindings: ResolvedKeybindingsConfig;
  readonly onAddTerminalContext: (selection: TerminalContextSelection) => void;
}

/**
 * Keeps terminal drawers mounted for recently open threads, so switching back
 * to one finds its sessions as they were. Every thread's terminal and draft
 * state feeds the reconcile, so it subscribes here instead of in ChatView.
 */
export function ChatTerminalDrawers(props: ChatTerminalDrawersProps) {
  const { activeThreadKey } = props;
  const openTerminalThreadKeys = useTerminalUiStateStore(
    useShallow((state) =>
      Object.entries(state.terminalUiStateByThreadKey).flatMap(
        ([nextThreadKey, nextTerminalUiState]) =>
          nextTerminalUiState.terminalOpen ? [nextThreadKey] : [],
      ),
    ),
  );
  const terminalStateThreadKeys = useTerminalUiStateStore(
    useShallow((state) => [
      ...Object.keys(state.terminalUiStateByThreadKey),
      ...Object.keys(state.suppressedTerminalIdsByThreadKey),
    ]),
  );
  const panelTerminalThreadKeys = useRightPanelStore(
    useShallow((state) =>
      Object.entries(state.byThreadKey).flatMap(([threadKey, panel]) =>
        panel.surfaces.some((surface) => surface.kind === "terminal") ? [threadKey] : [],
      ),
    ),
  );
  const serverThreadRefs = useThreadRefs();
  const serverThreadKeys = serverThreadRefs.map(scopedThreadKey);
  const draftThreadsByThreadKey = useComposerDraftStore((store) => store.draftThreadsByThreadKey);
  const draftThreadKeys = Object.values(draftThreadsByThreadKey).map((draftThread) =>
    scopedThreadKey(scopeThreadRef(draftThread.environmentId, draftThread.threadId)),
  );
  const { active: panelAnimationsActive, durationMs: panelAnimationDurationMs } =
    usePanelAnimationSettings();
  const activeTerminalDrawerPresence = usePanelPresence(
    Boolean(activeThreadKey && props.activeTerminalOpen),
    true,
    panelAnimationsActive,
    activeThreadKey,
    panelAnimationDurationMs,
  );
  const [mountedTerminalThreadKeys, setMountedTerminalThreadKeys] = useState<string[]>([]);
  const existingThreadKeys = new Set<string>([...serverThreadKeys, ...draftThreadKeys]);
  const existingOpenTerminalThreadKeys = openTerminalThreadKeys.filter((nextThreadKey) =>
    existingThreadKeys.has(nextThreadKey),
  );
  const reconciledMountedTerminalThreadKeys = reconcileMountedTerminalThreadIds({
    currentThreadIds: mountedTerminalThreadKeys,
    openThreadIds: existingOpenTerminalThreadKeys,
    activeThreadId: activeThreadKey,
    activeThreadTerminalOpen: activeTerminalDrawerPresence.present,
    maxHiddenThreadCount: MAX_HIDDEN_MOUNTED_TERMINAL_THREADS,
  });
  if (
    reconciledMountedTerminalThreadKeys.length !== mountedTerminalThreadKeys.length ||
    reconciledMountedTerminalThreadKeys.some(
      (threadKey, index) => threadKey !== mountedTerminalThreadKeys[index],
    )
  ) {
    setMountedTerminalThreadKeys(reconciledMountedTerminalThreadKeys);
  }
  const mountedTerminalThreadRefs = mountedTerminalThreadKeys.flatMap((mountedThreadKey) => {
    const mountedThreadRef = parseScopedThreadKey(mountedThreadKey);
    return mountedThreadRef ? [{ key: mountedThreadKey, threadRef: mountedThreadRef }] : [];
  });

  // Every thread with local terminal tabs follows the server, so closes seen while away are not missed.
  const reconciledThreadKeys = [
    ...new Set([
      ...terminalStateThreadKeys,
      ...panelTerminalThreadKeys,
      ...(activeThreadKey === null ? [] : [activeThreadKey]),
    ]),
  ].filter((threadKey) => existingThreadKeys.has(threadKey));

  return [
    ...reconciledThreadKeys.map((threadKey) => (
      <ThreadTerminalReconciler key={`reconcile:${threadKey}`} threadKey={threadKey} />
    )),
    ...mountedTerminalThreadRefs.map(({ key: mountedThreadKey, threadRef: mountedThreadRef }) => (
      <PersistentThreadTerminalDrawer
        key={mountedThreadKey}
        threadRef={mountedThreadRef}
        threadId={mountedThreadRef.threadId}
        active={mountedThreadKey === activeThreadKey}
        composerHeight={mountedThreadKey === activeThreadKey ? props.composerHeight : 0}
        launchContext={mountedThreadKey === activeThreadKey ? props.launchContext : null}
        splitShortcutLabel={props.splitShortcutLabel}
        splitVerticalShortcutLabel={props.splitVerticalShortcutLabel}
        newShortcutLabel={props.newShortcutLabel}
        closeShortcutLabel={props.closeShortcutLabel}
        keybindings={props.keybindings}
        onAddTerminalContext={props.onAddTerminalContext}
      />
    )),
  ];
}
