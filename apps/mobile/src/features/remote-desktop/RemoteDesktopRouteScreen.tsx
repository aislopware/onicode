import { useAtomValue } from "@effect/atom-react";
import { useIsFocused, useNavigation, type StaticScreenProps } from "@react-navigation/native";
import type { MenuAction } from "@react-native-menu/menu";
import { EnvironmentId } from "@onicode/contracts";
import { requireOptionalNativeModule } from "expo";
import * as Clipboard from "expo-clipboard";
import { activateKeepAwakeAsync, deactivateKeepAwake } from "expo-keep-awake";
import { msg, useGT, useMessages } from "gt-react-native";
import { useEffect, useEffectEvent, useRef, useState, type ReactNode } from "react";
import {
  REMOTE_DESKTOP_KEYBOARD_SENTINEL,
  remoteDesktopKeyboardEdit,
} from "@onicode/client-runtime/remote-desktop/protocol";
import {
  ScrollView,
  StatusBar,
  TextInput,
  useWindowDimensions,
  View,
  type TextInputInstance,
} from "react-native";
import { KeyboardEvents, KeyboardStickyView } from "react-native-keyboard-controller";
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
import type { RemoteDesktopTouchMode, RemoteDesktopView } from "./remote-desktop-document";
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
  { id: "copy", title: msg("Copy"), subtitle: "⌘C", codes: ["MetaLeft", "KeyC"] },
  { id: "paste", title: msg("Paste"), subtitle: "⌘V", codes: ["MetaLeft", "KeyV"] },
  { id: "undo", title: msg("Undo"), subtitle: "⌘Z", codes: ["MetaLeft", "KeyZ"] },
  { id: "select-all", title: msg("Select all"), subtitle: "⌘A", codes: ["MetaLeft", "KeyA"] },
  { id: "switcher", title: msg("App switcher"), subtitle: "⌘Tab", codes: ["MetaLeft", "Tab"] },
  { id: "spotlight", title: msg("Spotlight"), subtitle: "⌘Space", codes: ["MetaLeft", "Space"] },
  {
    id: "mission-control",
    title: msg("Mission Control"),
    subtitle: "⌃↑",
    codes: ["ControlLeft", "ArrowUp"],
  },
  {
    id: "lock",
    title: msg("Lock screen"),
    subtitle: "⌃⌘Q",
    codes: ["ControlLeft", "MetaLeft", "KeyQ"],
  },
] as const;

// Sticky: each holds until the next key, click or drag goes out with it. A second press locks it.
const MODIFIERS = [
  { code: "ControlLeft", label: "⌃", accessibilityLabel: "Control" },
  { code: "AltLeft", label: "⌥", accessibilityLabel: "Option" },
  { code: "ShiftLeft", label: "⇧", accessibilityLabel: "Shift" },
  { code: "MetaLeft", label: "⌘", accessibilityLabel: "Command" },
] as const;

// The toolbar's own padding, and its row of 48 point buttons.
const TOOLBAR_PADDING = 4;
const TOOLBAR_ROW_HEIGHT = 48;
// Sideways on a phone the toolbar stands at the side, where it costs width instead of height.
const RAIL_WIDTH = 56;
const RAIL_MAX_HEIGHT = 520;
const KEEP_AWAKE_TAG = "remote-desktop";

/** Where each display was last being read, so coming back to it does not start over. */
const lastViews = new Map<string, RemoteDesktopView>();

const nativeControls = () =>
  requireOptionalNativeModule<{ setFreeRotation?: (enabled: boolean) => Promise<void> }>(
    "OniNativeControls",
  );
// What a keyboard that reports no duration is given.
const KEYBOARD_MOVE_MS = 250;

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
  const gt = useGT();
  const m = useMessages();
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
  const [lockedModifiers, setLockedModifiers] = useState<ReadonlyArray<string>>([]);
  const [touchMode, setTouchMode] = useState<RemoteDesktopTouchMode>("trackpad");
  const [zoomed, setZoomed] = useState(false);
  const viewer = useRef<RemoteDesktopViewRef>(null);
  const [keyboardVisible, setKeyboardVisible] = useState(false);
  const window = useWindowDimensions();
  const sideways = window.width > window.height && window.height < RAIL_MAX_HEIGHT;
  const label = environment.presentation?.entry.target.label ?? gt("this machine");
  const control = host?.control === true;
  const showKeys = control && streaming;
  const toolbarPadding = Math.max(insets.bottom, TOOLBAR_PADDING);
  // With the keyboard up, the toolbar's safe-area padding tucks under it.
  const toolbarTuck = toolbarPadding - TOOLBAR_PADDING;
  const toolbarHeight = TOOLBAR_PADDING + TOOLBAR_ROW_HEIGHT + toolbarPadding;
  /** How much of the picture the keyboard and the toolbar riding it cover. */
  const covered = useRef(0);
  const coverPicture = useEffectEvent((keyboardHeight: number, durationMs: number) => {
    setKeyboardVisible(keyboardHeight > 0);
    // Sideways the toolbar is not below the picture until the keyboard brings it up over it.
    const toolbar = sideways && keyboardHeight > 0 ? toolbarHeight : 0;
    covered.current = Math.max(0, keyboardHeight - toolbarTuck) + toolbar;
    viewer.current?.setBottomInset(covered.current, durationMs > 0 ? durationMs : KEYBOARD_MOVE_MS);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- lets a phone turn sideways only while this screen is the one showing
  useEffect(() => {
    if (!focused) return;
    const controls = nativeControls();
    void controls?.setFreeRotation?.(true).catch(() => {});
    return () => void controls?.setFreeRotation?.(false).catch(() => {});
  }, [focused]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- follows the keyboard's own show and hide events, so the picture moves as the keyboard starts to
  useEffect(() => {
    const shown = KeyboardEvents.addListener("keyboardWillShow", (event) =>
      coverPicture(event.height, event.duration),
    );
    const hidden = KeyboardEvents.addListener("keyboardWillHide", (event) => {
      coverPicture(0, event.duration);
      // A keyboard the system closed leaves the field focused, and focusing it again does nothing.
      field.current?.blur();
    });
    return () => {
      shown.remove();
      hidden.remove();
    };
  }, []);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- holds the device awake for as long as a picture is streaming
  useEffect(() => {
    if (!streaming) return;
    const activation = activateKeepAwakeAsync(KEEP_AWAKE_TAG);
    void activation.catch(() => {});
    return () => void activation.then(() => deactivateKeepAwake(KEEP_AWAKE_TAG)).catch(() => {});
  }, [streaming]);

  const onStreamingChange = (next: boolean) => {
    setStreaming(next);
    // A new viewer starts with no modifiers held, and has not been told about the keyboard.
    if (next) viewer.current?.setBottomInset(covered.current, 0);
    else {
      setModifiers([]);
      setLockedModifiers([]);
      setZoomed(false);
    }
  };
  // Off, then held for one use, then locked until pressed again.
  const toggleModifier = (code: string) => {
    const wasLocked = lockedModifiers.includes(code);
    const held = wasLocked
      ? lockedModifiers.filter((candidate) => candidate !== code)
      : modifiers.includes(code)
        ? [...lockedModifiers, code]
        : lockedModifiers;
    const next = MODIFIERS.map((modifier) => modifier.code).filter((candidate) =>
      candidate === code ? !wasLocked : modifiers.includes(candidate),
    );
    setModifiers(next);
    setLockedModifiers(held);
    viewer.current?.setModifiers(next, held);
  };
  const pressKey = (code: string) => {
    viewer.current?.chord([...modifiers, code]);
    setModifiers(lockedModifiers);
  };
  // A WebView raises no keyboard for a field focused from outside it: never on Android, and on iOS
  // not until the picture has been touched. A native field raises it instead. A touch on the
  // picture then hands the WebView focus, and the viewer's own field takes the typing from there.
  const field = useRef<TextInputInstance>(null);
  const fieldText = useRef(REMOTE_DESKTOP_KEYBOARD_SENTINEL);
  const typeField = (text: string) => {
    const edit = remoteDesktopKeyboardEdit(fieldText.current, text);
    fieldText.current = edit.reset ? REMOTE_DESKTOP_KEYBOARD_SENTINEL : text;
    if (edit.reset) field.current?.setNativeProps({ text: REMOTE_DESKTOP_KEYBOARD_SENTINEL });
    viewer.current?.typeEdit(edit.backspaces, edit.lines);
  };
  const toggleKeyboard = () => {
    if (keyboardVisible) {
      // Blur the fields and nothing more: a native dismiss on top leaves iOS unable to raise the
      // keyboard again on the next press.
      field.current?.blur();
      viewer.current?.blurKeyboard();
    } else {
      viewer.current?.focusKeyboard();
      field.current?.focus();
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
            title: gt("Trackpad"),
            subtitle: gt("Drag to move the pointer, tap to click"),
            state: touchMode === "trackpad" ? ("on" as const) : undefined,
          },
          {
            id: "pointer:touch",
            title: gt("Direct touch"),
            subtitle: gt("Tap to click, hold then drag to drag"),
            state: touchMode === "touch" ? ("on" as const) : undefined,
          },
          { id: "paste", title: gt("Paste from this device") },
          {
            id: "shortcuts",
            title: gt("Shortcuts"),
            subactions: SHORTCUTS.map((shortcut) => ({
              id: `shortcut:${shortcut.id}`,
              title: m(shortcut.title),
              subtitle: shortcut.subtitle,
            })),
          },
        ]
      : []),
    ...(displayActions.length > 1
      ? [
          {
            id: "displays",
            title: gt("Display"),
            displayInline: !showKeys,
            subactions: displayActions,
          },
        ]
      : []),
  ];
  const runMenuAction = (id: string) => {
    const shortcut = SHORTCUTS.find((candidate) => id === `shortcut:${candidate.id}`);
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
        title={gt("Environment not found")}
        detail={gt("This environment is no longer saved on this device.")}
      />
    ) : (
      <Waiting message={gt("Opening remote desktop…")} />
    );
  } else if (!connected) {
    content = (
      <EnvironmentConnectionNotice
        environmentLabel={label}
        connection={environment.presentation.connection}
        resource="remoteDesktop"
        onRetry={() => void retryEnvironment(environmentId)}
      />
    );
  } else if (config === null) {
    content = <Waiting message={gt("Connecting to {label}…", { label })} />;
  } else if (!supported) {
    content = (
      <EmptyState
        variant="plain"
        title={gt("Update Onicode on that machine")}
        detail={gt(
          "The version of Onicode on {label} cannot share its screen. Update it there to use remote desktop.",
          { label },
        )}
      />
    );
  } else if (!canConnect) {
    content =
      session.data === null && session.error === null ? (
        <Waiting message={gt("Checking permissions…")} />
      ) : (
        <EmptyState
          variant="plain"
          title={gt("Remote desktop unavailable")}
          detail={
            session.error ??
            gt(
              "This connection does not have permission to use terminals, which remote desktop needs. Pair this device again with that permission.",
            )
          }
        />
      );
  } else if (state.error !== null) {
    content = (
      <EmptyState
        variant="plain"
        title={gt("Could not check for a shared screen")}
        detail={state.error}
        action={<Button label={gt("Retry")} tone="secondary" onPress={state.refresh} />}
      />
    );
  } else if (state.data === null) {
    content = <Waiting message={gt("Checking for a shared screen…")} />;
  } else if (host === null) {
    content = (
      <EmptyState
        variant="plain"
        title={gt("Screen sharing is off")}
        detail={gt("Open Onicode on that machine and turn on Remote desktop in Settings.")}
      />
    );
  } else if (display === null) {
    content = (
      <EmptyState
        variant="plain"
        title={gt("No display to show")}
        detail={gt("Onicode on {label} found no display to share.", { label })}
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
        display={{
          width: display.width,
          height: display.height,
          // A host older than the field: its high-resolution displays are the wide ones.
          scaleFactor: display.scaleFactor ?? (display.width > 2000 ? 2 : 1),
        }}
        view={lastViews.get(`${environmentId}:${display.id}`)}
        retriesItself={attempt === 0}
        background={background}
        onStreamingChange={onStreamingChange}
        onModifiersUsed={() => setModifiers(lockedModifiers)}
        onZoomedChange={setZoomed}
        onViewChange={(view) => lastViews.set(`${environmentId}:${display.id}`, view)}
        onRetry={() => setAttempt((current) => current + 1)}
      />
    );
  } else {
    content = null;
  }

  const keyButtons = [
    ...KEYS.map((key) => ({ ...key, modifier: false })),
    ...MODIFIERS.map((key) => ({ ...key, modifier: true })),
    ...MORE_KEYS.map((key) => ({ ...key, modifier: false })),
  ].map((key) => {
    const locked = key.modifier && lockedModifiers.includes(key.code);
    return (
      <ComposerToolbarButton
        key={key.code}
        label={key.label}
        accessibilityLabel={
          locked
            ? gt("{modifier}, locked", { modifier: key.accessibilityLabel })
            : key.accessibilityLabel
        }
        variant={locked ? "primary" : "default"}
        // Screen readers hear armed and locked keys as selected; the label adds "locked".
        selected={key.modifier ? modifiers.includes(key.code) : undefined}
        // Held for one use: tinted, so it reads as armed but lighter than locked.
        className={
          key.modifier && !locked && modifiers.includes(key.code)
            ? "border-primary/60 bg-primary/25"
            : undefined
        }
        minWidth={44}
        showChevron={false}
        style={sideways && !keyboardVisible ? { width: 48, paddingHorizontal: 0 } : undefined}
        onPress={() => (key.modifier ? toggleModifier(key.code) : pressKey(key.code))}
      />
    );
  });
  const closeButton = (
    <SheetCloseButton accessibilityLabel={gt("Close remote desktop")} onClose={onClose} />
  );
  const viewButtons = (
    <>
      {streaming ? (
        <IconButton
          icon={zoomed ? "minus.magnifyingglass" : "plus.magnifyingglass"}
          accessibilityLabel={zoomed ? gt("Show the whole screen") : gt("Zoom in to read")}
          onPress={() => viewer.current?.toggleZoom()}
        />
      ) : null}
      {menuActions.length > 0 ? (
        <Menu
          actions={menuActions}
          isAnchoredToRight
          onPressAction={({ nativeEvent }) => runMenuAction(nativeEvent.event)}
        >
          <IconButton icon="ellipsis" accessibilityLabel={gt("Remote desktop options")} />
        </Menu>
      ) : null}
      {showKeys ? (
        <IconButton
          icon={
            keyboardVisible
              ? { ios: "keyboard.chevron.compact.down", android: "keyboard_hide" }
              : "keyboard"
          }
          accessibilityLabel={keyboardVisible ? gt("Hide keyboard") : gt("Show keyboard")}
          onPress={toggleKeyboard}
        />
      ) : null}
    </>
  );
  // Sideways the bar waits below the screen and comes up with the keyboard; the rail does its job until then.
  const barHidden = sideways && !keyboardVisible;

  return (
    <View
      className="flex-1"
      style={{
        // Sideways with the keyboard up the rail's column is empty, so it joins the picture's black.
        backgroundColor: sideways && keyboardVisible ? "#000000" : background,
        paddingTop: insets.top,
        paddingLeft: sideways ? 0 : insets.left,
        paddingRight: insets.right + (sideways ? RAIL_WIDTH : 0),
      }}
    >
      <StatusBar hidden animated />
      {showKeys ? (
        <TextInput
          ref={field}
          multiline
          autoCapitalize="none"
          autoCorrect={false}
          spellCheck={false}
          caretHidden
          contextMenuHidden
          // Sideways, Android would otherwise cover the picture with a full-screen editor.
          disableFullscreenUI
          importantForAutofill="no"
          importantForAccessibility="no-hide-descendants"
          defaultValue={REMOTE_DESKTOP_KEYBOARD_SENTINEL}
          onChangeText={typeField}
          style={{ position: "absolute", top: 0, left: 0, width: 1, height: 1, opacity: 0 }}
        />
      ) : null}
      {/* The picture keeps its size when the keyboard opens; the toolbar rides up over it. */}
      <View
        className="flex-1 justify-center"
        style={
          // Sideways the strip beside the camera continues the picture's black, not the toolbar's colour.
          sideways
            ? { paddingLeft: insets.left, backgroundColor: "#000000" }
            : { marginBottom: toolbarHeight }
        }
      >
        {content}
      </View>
      {barHidden ? (
        <View
          className="absolute items-center"
          style={{
            top: insets.top,
            bottom: Math.max(insets.bottom, TOOLBAR_PADDING),
            right: insets.right,
            width: RAIL_WIDTH,
          }}
        >
          {closeButton}
          {showKeys ? (
            <ScrollView
              keyboardShouldPersistTaps="always"
              showsVerticalScrollIndicator={false}
              className="flex-1"
              contentContainerClassName="items-center gap-1.5 py-1"
            >
              {keyButtons}
            </ScrollView>
          ) : (
            <View className="flex-1" />
          )}
          {viewButtons}
        </View>
      ) : null}
      <KeyboardStickyView
        offset={{ closed: sideways ? toolbarHeight : 0, opened: toolbarTuck }}
        style={{ position: "absolute", bottom: 0, left: insets.left, right: insets.right }}
      >
        <View
          className="flex-row items-center gap-1 px-1"
          pointerEvents={barHidden ? "none" : "auto"}
          accessibilityElementsHidden={barHidden}
          importantForAccessibility={barHidden ? "no-hide-descendants" : "auto"}
          style={{
            backgroundColor: background,
            paddingTop: TOOLBAR_PADDING,
            paddingBottom: toolbarPadding,
          }}
        >
          {closeButton}
          {showKeys ? (
            <ScrollView
              horizontal
              keyboardShouldPersistTaps="always"
              showsHorizontalScrollIndicator={false}
              className="flex-1"
              contentContainerClassName="items-center gap-1.5 px-1"
            >
              {keyButtons}
            </ScrollView>
          ) : (
            <AppText numberOfLines={1} className="flex-1 px-1 text-sm text-foreground-muted">
              {host !== null && !control && streaming ? gt("Input is off on the host") : label}
            </AppText>
          )}
          {viewButtons}
        </View>
      </KeyboardStickyView>
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
