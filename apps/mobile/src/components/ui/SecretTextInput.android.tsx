import { requireNativeView } from "expo";
import { useState, type Ref } from "react";
import { processColor, View, type NativeSyntheticEvent, type ViewProps } from "react-native";

import { flattenThemeColor } from "../../lib/mobileTheme";
import { useUniwindTheme } from "../../lib/useUniwindTheme";
import { AppTextInput } from "./AppText";
import { SecretRevealButton } from "./SecretRevealButton";
import type { SecretTextInputProps } from "./SecretTextInput.types";

interface NativeSecretInputRef {
  focus: () => Promise<void>;
}

interface NativeSecretInputProps extends ViewProps {
  readonly ref?: Ref<NativeSecretInputRef>;
  readonly inputLabel: string;
  readonly value: string;
  readonly placeholder: string;
  readonly textColor: number;
  readonly placeholderColor: number;
  readonly cursorColor: number;
  readonly fontSize: number;
  readonly editable: boolean;
  readonly revealed: boolean;
  readonly returnKey: "next" | "done";
  readonly onChangeText: (event: NativeSyntheticEvent<{ readonly value: string }>) => void;
  readonly onSubmit: (event: NativeSyntheticEvent<{ readonly value: string }>) => void;
}

const NativeSecretInput = requireNativeView<NativeSecretInputProps>("OniSecretInput");

const FONT_SIZE = 16;
const LINE_HEIGHT = 24;

function nativeColor(value: string) {
  const color = processColor(value);
  return typeof color === "number" ? color : 0;
}

/**
 * A native EditText a password manager can fill. React Native's TextInput reports what is typed to
 * Android's autofill session, so a manager could offer to save it. The frame matches AppTextInput's.
 */
export function SecretTextInput({ onMount, secret = true, ...props }: SecretTextInputProps) {
  const theme = useUniwindTheme();
  const surface = theme["--color-input"];
  const [revealed, setRevealed] = useState(false);
  // The native field always offers a login's password; a field marked not secret, such as an
  // email, takes the username instead.
  if (!secret) {
    const returnKeyType = props.returnKeyType ?? "done";
    return (
      <AppTextInput
        ref={onMount}
        accessibilityLabel={props.accessibilityLabel}
        placeholder={props.placeholder}
        value={props.value}
        onChangeText={props.onChangeText}
        editable={props.editable}
        autoCorrect={false}
        autoCapitalize="none"
        autoComplete="username"
        spellCheck={false}
        returnKeyType={returnKeyType}
        submitBehavior={returnKeyType === "next" ? "submit" : "blurAndSubmit"}
        onSubmitEditing={(event) => props.onSubmitEditing(event.nativeEvent.text)}
      />
    );
  }
  return (
    <View className="min-h-13.5 flex-row items-center gap-1 rounded-2xl border border-input-border bg-input pr-1.5 pl-3.5">
      <NativeSecretInput
        ref={
          onMount === undefined
            ? undefined
            : (native: NativeSecretInputRef | null) =>
                onMount(
                  native === null
                    ? null
                    : { focus: () => void native.focus().catch(() => undefined) },
                )
        }
        inputLabel={props.accessibilityLabel}
        value={props.value}
        placeholder={props.placeholder}
        textColor={nativeColor(flattenThemeColor(theme["--color-foreground"], surface))}
        placeholderColor={nativeColor(flattenThemeColor(theme["--color-placeholder"], surface))}
        cursorColor={nativeColor(flattenThemeColor(theme["--color-focus"], surface))}
        fontSize={FONT_SIZE}
        editable={props.editable}
        revealed={revealed}
        returnKey={props.returnKeyType ?? "done"}
        onChangeText={(event) => props.onChangeText(event.nativeEvent.value)}
        onSubmit={(event) => props.onSubmitEditing(event.nativeEvent.value)}
        style={{ flex: 1, height: LINE_HEIGHT }}
      />
      <SecretRevealButton revealed={revealed} onPress={() => setRevealed(!revealed)} />
    </View>
  );
}
