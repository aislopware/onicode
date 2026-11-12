import remoteDesktopScript from "@onicode/mobile-remote-desktop";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import type { EnvironmentId } from "@onicode/contracts";
import * as Clipboard from "expo-clipboard";
import {
  useEffectEvent,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
} from "react";
import { Platform, View } from "react-native";
import { WebView } from "react-native-webview";

import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { AppText } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { remoteDesktopEnvironment } from "../../state/remoteDesktop";
import { useAtomCommand } from "../../state/use-atom-command";
import {
  REMOTE_DESKTOP_GLOBAL,
  remoteDesktopBaseUrl,
  remoteDesktopDocument,
  remoteDesktopMessage,
  type RemoteDesktopTouchMode,
} from "./remote-desktop-document";

export interface RemoteDesktopViewRef {
  /** Presses the keys in order and releases them in reverse. */
  chord: (codes: ReadonlyArray<string>) => void;
  setModifiers: (codes: ReadonlyArray<string>) => void;
  focusKeyboard: () => void;
  blurKeyboard: () => void;
  setTouchMode: (mode: RemoteDesktopTouchMode) => void;
  /** Pastes this device's clipboard text into the host. */
  paste: (text: string) => void;
}

// The viewer letterboxes the host's picture in black, so the page behind it matches.
const PICTURE_BACKGROUND = "#000000";

export function RemoteDesktopWebView({
  ref,
  environmentId,
  displayId,
  control,
  touchMode,
  onStreamingChange,
  onModifiersUsed,
  onRetry,
  background,
}: {
  readonly ref?: Ref<RemoteDesktopViewRef>;
  readonly environmentId: EnvironmentId;
  readonly displayId: string;
  readonly control: boolean;
  /** The mode to start in; later changes go through the ref, without reloading the viewer. */
  readonly touchMode: RemoteDesktopTouchMode;
  readonly onStreamingChange: (streaming: boolean) => void;
  readonly onModifiersUsed: () => void;
  readonly onRetry: () => void;
  /** Behind the status shown while there is no picture. */
  readonly background: string;
}) {
  const webView = useRef<WebView<object>>(null);
  const active = useRef(false);
  const failed = useRef(false);
  const bootstrapTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [status, setStatus] = useState<"connecting" | "streaming" | "reconnecting" | "error">(
    "connecting",
  );
  const [initialTouchMode] = useState(touchMode);
  const [error, setError] = useState<string | null>(null);
  const connect = useAtomCommand(remoteDesktopEnvironment.connect, { reportFailure: false });
  const inject = (call: string) => {
    if (active.current)
      webView.current?.injectJavaScript(`window.${REMOTE_DESKTOP_GLOBAL}?.${call}; true;`);
  };
  const clearBootstrapTimer = () => {
    if (bootstrapTimer.current === null) return;
    clearTimeout(bootstrapTimer.current);
    bootstrapTimer.current = null;
  };
  const fail = (message: string) => {
    if (!active.current || failed.current) return;
    failed.current = true;
    clearBootstrapTimer();
    inject("stop()");
    setError(message);
    setStatus("error");
    onStreamingChange(false);
  };
  const bootstrapTimedOut = useEffectEvent(() =>
    fail("The remote desktop viewer could not start. Retry to connect again."),
  );
  const streamEnded = useEffectEvent(() => onStreamingChange(false));
  // The connection lives exactly as long as this view.
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- starts the bootstrap timeout and stops the WebView's peer connection when this view unmounts
  useLayoutEffect(() => {
    active.current = true;
    const view = webView.current;
    bootstrapTimer.current = setTimeout(bootstrapTimedOut, 15_000);
    return () => {
      if (bootstrapTimer.current !== null) clearTimeout(bootstrapTimer.current);
      bootstrapTimer.current = null;
      active.current = false;
      view?.injectJavaScript(`window.${REMOTE_DESKTOP_GLOBAL}?.stop(); true;`);
      streamEnded();
    };
  }, []);
  useImperativeHandle(ref, () => ({
    chord: (codes) => inject(`chord(${JSON.stringify(codes)})`),
    setModifiers: (codes) => inject(`setModifiers(${JSON.stringify(codes)})`),
    focusKeyboard: () => {
      // Android raises the keyboard for a focused field only while the WebView itself has focus.
      if (Platform.OS === "android") webView.current?.requestFocus();
      inject("focusKeyboard()");
    },
    blurKeyboard: () => inject("blurKeyboard()"),
    setTouchMode: (mode) => inject(`setTouchMode(${JSON.stringify(mode)})`),
    paste: (text) => inject(`paste(${JSON.stringify(text)})`),
  }));
  const signal = async (offer: string) => {
    const result = await connect({ environmentId, input: { displayId, offer } });
    if (!active.current || failed.current) return;
    if (result._tag === "Success") {
      inject(`answer(${JSON.stringify(result.value.answer)})`);
      return;
    }
    const cause = squashAtomCommandFailure(result);
    // RemoteDesktopError's message is written for the person viewing.
    const message =
      cause instanceof Error && cause.message.trim().length > 0
        ? cause.message
        : "Could not reach the host. Retry to connect again.";
    inject(`fail(${JSON.stringify(message)})`);
  };
  const processTerminated = () =>
    fail("The remote desktop viewer stopped. Retry to connect again.");
  const source = {
    html: remoteDesktopDocument(
      JSON.stringify({ control, touchMode: initialTouchMode }),
      remoteDesktopScript,
    ),
    baseUrl: remoteDesktopBaseUrl(Platform.OS),
  };
  return (
    <View className="flex-1" style={{ backgroundColor: PICTURE_BACKGROUND }}>
      <WebView<object>
        ref={webView}
        source={source}
        originWhitelist={["*"]}
        scrollEnabled={false}
        bounces={false}
        overScrollMode="never"
        mixedContentMode="always"
        allowUniversalAccessFromFileURLs
        contentInsetAdjustmentBehavior="never"
        automaticallyAdjustContentInsets={false}
        setSupportMultipleWindows={false}
        allowsInlineMediaPlayback
        mediaPlaybackRequiresUserAction={false}
        keyboardDisplayRequiresUserAction={false}
        hideKeyboardAccessoryView
        style={{ flex: 1, backgroundColor: PICTURE_BACKGROUND }}
        onError={() => fail("The remote desktop viewer could not load. Retry to connect again.")}
        onHttpError={() =>
          fail("The remote desktop viewer could not load. Retry to connect again.")
        }
        onContentProcessDidTerminate={processTerminated}
        onRenderProcessGone={processTerminated}
        onShouldStartLoadWithRequest={(request) =>
          request.url === "about:blank" || request.url === source.baseUrl
        }
        onMessage={(event) => {
          if (!active.current || failed.current) return;
          const message = remoteDesktopMessage(event.nativeEvent.data);
          if (message?.type === "offer") void signal(message.sdp);
          else if (message?.type === "modifiersUsed") onModifiersUsed();
          else if (message?.type === "clipboard") void Clipboard.setStringAsync(message.text);
          else if (message?.type === "status") {
            clearBootstrapTimer();
            if (message.status === "error") {
              fail(message.detail ?? "The remote desktop connection failed.");
              return;
            }
            setStatus(message.status);
            // The toolbar stays through a reconnect; the picture is still the one it controls.
            onStreamingChange(message.status !== "connecting");
          }
        }}
      />
      {status !== "streaming" ? (
        <View
          className="absolute inset-0 items-center justify-center gap-4 px-6"
          style={{ backgroundColor: status === "reconnecting" ? "#000000aa" : background }}
        >
          {status !== "error" ? <AppActivityIndicator colorClassName="accent-icon" /> : null}
          <AppText
            selectable={status === "error"}
            accessibilityLiveRegion="polite"
            className="text-center text-sm text-foreground-muted"
          >
            {status === "error"
              ? error
              : status === "reconnecting"
                ? "Reconnecting..."
                : "Connecting to the host..."}
          </AppText>
          {status === "error" ? (
            <View className="items-center">
              <Button label="Retry" tone="secondary" onPress={onRetry} />
            </View>
          ) : null}
        </View>
      ) : null}
    </View>
  );
}
