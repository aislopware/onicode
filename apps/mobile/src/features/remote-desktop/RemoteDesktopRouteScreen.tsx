import { useAtomValue } from "@effect/atom-react";
import { useIsFocused, useNavigation, type StaticScreenProps } from "@react-navigation/native";
import type { MenuAction } from "@react-native-menu/menu";
import { EnvironmentId } from "@onicode/contracts";
import * as Clipboard from "expo-clipboard";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ScrollView, StatusBar, View } from "react-native";
import { KeyboardController, useKeyboardState } from "react-native-keyboard-controller";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { ComposerToolbarButton } from "../../components/ComposerToolbar";
import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { AppText } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { IconButton } from "../../components/ui/IconButton";
import { Menu } from "../../components/ui/Menu";
import { SheetCloseButton } from "../../components/ui/SheetHeader";
import { useAppForeground } from "../../lib/appState";
import { useEnvironmentPresentation } from "../../state/presentation";
import { useEnvironmentQuery } from "../../state/query";
import { remoteDesktopEnvironment } from "../../state/remoteDesktop";
import { serverEnvironment } from "../../state/server";
import { environmentSession } from "../../state/session";
import { EnvironmentConnectionNotice } from "../connection/EnvironmentConnectionNotice";
import { useRetryEnvironment } from "../connection/useRetryEnvironment";
import { useAppearancePreferences } from "../settings/appearance/AppearancePreferencesProvider";
import type { RemoteDesktopTouchMode } from "./remote-desktop-document";
import { RemoteDesktopWebView, type RemoteDesktopViewRef } from "./RemoteDesktopWebView";

const KEYS = [
  { code: "Escape", label: "esc", accessibilityLabel: "Escape" },
  { code: "Tab", label: "tab", accessibilityLabel: "Tab" },
  { code: "ArrowLeft", label: "←", accessibilityLabel: "Left arrow" },
  { code: "ArrowUp", label: "↑", accessibilityLabel: "Up arrow" },
  { code: "ArrowDown", label: "↓", accessibilityLabel: "Down arrow" },
  { code: "ArrowRight", label: "→", accessibilityLabel: "Right arrow" },
] as const;

const MORE_KEYS = [
  { code: "Delete", label: "⌦", accessibilityLabel: "Forward delete" },
  { code: "Home", label: "home", accessibilityLabel: "Home" },
  { code: "End", label: "end", accessibilityLabel: "End" },
  { code: "PageUp", label: "pg↑", accessibilityLabel: "Page up" },
  { code: "PageDown", label: "pg↓", accessibilityLabel: "Page down" },
  ...Array.from({ length: 12 }, (_, index) => ({
    code: `F${index + 1}`,
    label: `F${index + 1}`,
    accessibilityLabel: `F${index + 1}`,
  })),
];

// Chords a phone keyboard cannot type, or that take three taps on the strip.
const SHORTCUTS = [
  { title: "Copy", subtitle: "⌘C", codes: ["MetaLeft", "KeyC"] },
  { title: "Paste", subtitle: "⌘V", codes: ["MetaLeft", "KeyV"] },
  { title: "Undo", subtitle: "⌘Z", codes: ["MetaLeft", "KeyZ"] },
  { title: "Select all", subtitle: "⌘A", codes: ["MetaLeft", "KeyA"] },
  { title: "App switcher", subtitle: "⌘Tab", codes: ["MetaLeft", "Tab"] },
  { title: "Spotlight", subtitle: "⌘Space", codes: ["MetaLeft", "Space"] },
  { title: "Mission Control", subtitle: "⌃↑", codes: ["ControlLeft", "ArrowUp"] },
  { title: "Lock screen", subtitle: "⌃⌘Q", codes: ["ControlLeft", "MetaLeft", "KeyQ"] },
] as const;

// Sticky: each holds until the next key, from this strip or the soft keyboard, goes out with it.
const MODIFIERS = [
  { code: "ControlLeft", label: "⌃", accessibilityLabel: "Control" },
  { code: "AltLeft", label: "⌥", accessibilityLabel: "Option" },
  { code: "ShiftLeft", label: "⇧", accessibilityLabel: "Shift" },
  { code: "MetaLeft", label: "⌘", accessibilityLabel: "Command" },
] as const;

type RemoteDesktopRouteScreenProps = StaticScreenProps<{
  readonly environmentId: string;
}>;

export function RemoteDesktopRouteScreen({ route }: RemoteDesktopRouteScreenProps) {
  const navigation = useNavigation();
  const onClose = () => navigation.goBack();
  const { environmentId } = route.params;
  const isBlankLink = environmentId.trim().length === 0;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- pops a deep-linked route with a blank environment off the navigator
  useEffect(() => {
    if (isBlankLink) onClose();
  }, [isBlankLink, onClose]);
  if (isBlankLink) return null;
  return (
    <RemoteDesktopScreen
      key={environmentId}
      environmentId={EnvironmentId.make(environmentId)}
      onClose={onClose}
    />
  );
}

function RemoteDesktopScreen({
  environmentId,
  onClose,
}: {
  readonly environmentId: EnvironmentId;
  readonly onClose: () => void;
}) {
  const insets = useSafeAreaInsets();
  const { themeVariables } = useAppearancePreferences();
  const background = themeVariables["--color-sheet-solid"];
  const focused = useIsFocused();
  const foreground = useAppForeground();
  const environment = useEnvironmentPresentation(environmentId);
  const retryEnvironment = useRetryEnvironment();
  const config = useAtomValue(serverEnvironment.configValueAtom(environmentId));
  const connected = environment.presentation?.connection.phase === "connected";
  const supported = config?.remoteDesktop === true;
  const session = useEnvironmentQuery(
    connected ? environmentSession.sessionStateAtom(environmentId) : null,
  );
  const canConnect = useAtomValue(remoteDesktopEnvironment.connect.permissionAtom(environmentId));
  const state = useEnvironmentQuery(
    connected && supported ? remoteDesktopEnvironment.state({ environmentId, input: {} }) : null,
  );
  const host = state.data?.host ?? null;
  const [selectedDisplayId, setSelectedDisplayId] = useState<string | null>(null);
  const display =
    host?.displays.find((candidate) => candidate.id === selectedDisplayId) ??
    host?.displays.find((candidate) => candidate.primary) ??
    host?.displays[0] ??
    null;
  const [attempt, setAttempt] = useState(0);
  const [streaming, setStreaming] = useState(false);
  const [modifiers, setModifiers] = useState<ReadonlyArray<string>>([]);
  const [touchMode, setTouchMode] = useState<RemoteDesktopTouchMode>("trackpad");
  const viewer = useRef<RemoteDesktopViewRef>(null);
  const keyboardVisible = useKeyboardState((keyboard) => keyboard.isVisible);
  const keyboardHeight = useKeyboardState((keyboard) => keyboard.height);
  const label = environment.presentation?.entry.target.label ?? "this machine";
  const control = host?.control === true;
  const showKeys = control && streaming;

  const onStreamingChange = (next: boolean) => {
    setStreaming(next);
    // A new viewer starts with no modifiers held.
    if (!next) setModifiers([]);
  };
  const toggleModifier = (code: string) => {
    const next = MODIFIERS.map((modifier) => modifier.code).filter((candidate) =>
      candidate === code ? !modifiers.includes(candidate) : modifiers.includes(candidate),
    );
    setModifiers(next);
    viewer.current?.setModifiers(next);
  };
  const pressKey = (code: string) => {
    viewer.current?.chord([...modifiers, code]);
    setModifiers([]);
  };
  const toggleKeyboard = () => {
    if (keyboardVisible) {
      viewer.current?.blurKeyboard();
      void KeyboardController.dismiss();
    } else {
      viewer.current?.focusKeyboard();
    }
  };
  const chooseTouchMode = (mode: RemoteDesktopTouchMode) => {
    setTouchMode(mode);
    viewer.current?.setTouchMode(mode);
  };
  const pasteClipboard = async () => {
    const text = await Clipboard.getStringAsync().catch(() => "");
    if (text.length > 0) viewer.current?.paste(text);
  };
  const displayActions: MenuAction[] = (host?.displays ?? []).map((candidate) => ({
    id: `display:${candidate.id}`,
    title: candidate.label,
    subtitle: `${candidate.width} × ${candidate.height}`,
    state: candidate.id === display?.id ? "on" : undefined,
  }));
  const menuActions: MenuAction[] = [
    ...(showKeys
      ? [
          {
            id: "pointer:trackpad",
            title: "Trackpad",
            subtitle: "Drag to move the pointer, tap to click",
            state: touchMode === "trackpad" ? ("on" as const) : undefined,
          },
          {
            id: "pointer:touch",
            title: "Direct touch",
            subtitle: "Tap where you want to click",
            state: touchMode === "touch" ? ("on" as const) : undefined,
          },
          { id: "paste", title: "Paste from this device" },
          {
            id: "shortcuts",
            title: "Shortcuts",
            subactions: SHORTCUTS.map((shortcut) => ({
              id: `shortcut:${shortcut.title}`,
              title: shortcut.title,
              subtitle: shortcut.subtitle,
            })),
          },
        ]
      : []),
    ...(displayActions.length > 1
      ? [{ id: "displays", title: "Display", displayInline: !showKeys, subactions: displayActions }]
      : []),
  ];
  const runMenuAction = (id: string) => {
    const shortcut = SHORTCUTS.find((candidate) => id === `shortcut:${candidate.title}`);
    if (shortcut) viewer.current?.chord(shortcut.codes);
    else if (id === "paste") void pasteClipboard();
    else if (id === "pointer:trackpad") chooseTouchMode("trackpad");
    else if (id === "pointer:touch") chooseTouchMode("touch");
    else if (id.startsWith("display:")) setSelectedDisplayId(id.slice("display:".length));
  };

  let content: ReactNode;
  if (environment.presentation === null) {
    content = environment.isReady ? (
      <EmptyState
        variant="plain"
        title="Environment not found"
        detail="This environment is no longer saved on this device."
      />
    ) : (
      <Waiting message="Opening remote desktop…" />
    );
  } else if (!connected) {
    content = (
      <EnvironmentConnectionNotice
        environmentLabel={label}
        connection={environment.presentation.connection}
        resourceName="remote desktop"
        onRetry={() => void retryEnvironment(environmentId)}
      />
    );
  } else if (config === null) {
    content = <Waiting message={`Connecting to ${label}…`} />;
  } else if (!supported) {
    content = (
      <EmptyState
        variant="plain"
        title="Update Onicode on that machine"
        detail={`The version of Onicode on ${label} cannot share its screen. Update it there to use remote desktop.`}
      />
    );
  } else if (!canConnect) {
    content =
      session.data === null && session.error === null ? (
        <Waiting message="Checking permissions…" />
      ) : (
        <EmptyState
          variant="plain"
          title="Remote desktop unavailable"
          detail={
            session.error ??
            "This connection does not have permission to use terminals, which remote desktop needs. Pair this device again with that permission."
          }
        />
      );
  } else if (state.error !== null) {
    content = (
      <EmptyState
        variant="plain"
        title="Could not check for a shared screen"
        detail={state.error}
        action={<Button label="Retry" tone="secondary" onPress={state.refresh} />}
      />
    );
  } else if (state.data === null) {
    content = <Waiting message="Checking for a shared screen…" />;
  } else if (host === null) {
    content = (
      <EmptyState
        variant="plain"
        title="Screen sharing is off"
        detail="Open Onicode on that machine and turn on Remote desktop in Settings."
      />
    );
  } else if (display === null) {
    content = (
      <EmptyState
        variant="plain"
        title="No display to show"
        detail={`Onicode on ${label} found no display to share.`}
      />
    );
  } else if (focused && foreground) {
    content = (
      <RemoteDesktopWebView
        key={`${display.id}:${String(control)}:${attempt}`}
        ref={viewer}
        environmentId={environmentId}
        displayId={display.id}
        control={control}
        touchMode={touchMode}
        background={background}
        onStreamingChange={onStreamingChange}
        onModifiersUsed={() => setModifiers([])}
        onRetry={() => setAttempt((current) => current + 1)}
      />
    );
  } else {
    content = null;
  }

  return (
    <View
      className="flex-1"
      style={{
        backgroundColor: background,
        paddingTop: insets.top,
        paddingLeft: insets.left,
        paddingRight: insets.right,
      }}
    >
      <StatusBar hidden animated />
      <View className="flex-1 justify-center">{content}</View>
      <View
        className="flex-row items-center gap-1 px-1 pt-1"
        style={{ paddingBottom: keyboardVisible ? 4 : Math.max(insets.bottom, 4) }}
      >
        <SheetCloseButton accessibilityLabel="Close remote desktop" onClose={onClose} />
        {showKeys ? (
          <ScrollView
            horizontal
            keyboardShouldPersistTaps="always"
            showsHorizontalScrollIndicator={false}
            className="flex-1"
            contentContainerClassName="items-center gap-1.5 px-1"
          >
            {KEYS.map((key) => (
              <ComposerToolbarButton
                key={key.code}
                label={key.label}
                accessibilityLabel={key.accessibilityLabel}
                minWidth={44}
                showChevron={false}
                onPress={() => pressKey(key.code)}
              />
            ))}
            {MODIFIERS.map((modifier) => (
              <ComposerToolbarButton
                key={modifier.code}
                label={modifier.label}
                accessibilityLabel={modifier.accessibilityLabel}
                active={modifiers.includes(modifier.code)}
                minWidth={44}
                showChevron={false}
                onPress={() => toggleModifier(modifier.code)}
              />
            ))}
            {MORE_KEYS.map((key) => (
              <ComposerToolbarButton
                key={key.code}
                label={key.label}
                accessibilityLabel={key.accessibilityLabel}
                minWidth={44}
                showChevron={false}
                onPress={() => pressKey(key.code)}
              />
            ))}
          </ScrollView>
        ) : (
          <AppText numberOfLines={1} className="flex-1 px-1 text-sm text-foreground-muted">
            {host !== null && !control && streaming ? "Input is off on the host" : label}
          </AppText>
        )}
        {menuActions.length > 0 ? (
          <Menu
            actions={menuActions}
            isAnchoredToRight
            onPressAction={({ nativeEvent }) => runMenuAction(nativeEvent.event)}
          >
            <IconButton icon="ellipsis" accessibilityLabel="Remote desktop options" />
          </Menu>
        ) : null}
        {showKeys ? (
          <IconButton
            icon={
              keyboardVisible
                ? { ios: "keyboard.chevron.compact.down", android: "keyboard_hide" }
                : "keyboard"
            }
            accessibilityLabel={keyboardVisible ? "Hide keyboard" : "Show keyboard"}
            onPress={toggleKeyboard}
          />
        ) : null}
      </View>
      <View style={{ height: keyboardVisible ? keyboardHeight : 0 }} />
    </View>
  );
}

function Waiting({ message }: { readonly message: string }) {
  return (
    <View className="items-center gap-4 px-6">
      <AppActivityIndicator colorClassName="accent-icon" />
      <AppText className="text-center text-sm text-foreground-muted">{message}</AppText>
    </View>
  );
}
