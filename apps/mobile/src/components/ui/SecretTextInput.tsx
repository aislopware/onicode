import { useState } from "react";
import { View } from "react-native";

import { AppTextInput } from "./AppText";
import { SecretRevealButton } from "./SecretRevealButton";
import type { SecretTextInputProps } from "./SecretTextInput.types";

/** Masked field a password manager can fill; keyboard suggestions stay off. */
export function SecretTextInput({ onMount, secret = true, ...props }: SecretTextInputProps) {
  const [revealed, setRevealed] = useState(false);
  const returnKeyType = props.returnKeyType ?? "done";
  return (
    <View className="justify-center">
      <AppTextInput
        ref={onMount}
        className={secret ? "pr-12" : undefined}
        accessibilityLabel={props.accessibilityLabel}
        placeholder={props.placeholder}
        value={props.value}
        onChangeText={props.onChangeText}
        editable={props.editable}
        secureTextEntry={secret && !revealed}
        autoCorrect={false}
        autoCapitalize="none"
        // Puts the Passwords key above the keyboard, which opens the person's password manager.
        autoComplete="current-password"
        textContentType="password"
        spellCheck={false}
        returnKeyType={returnKeyType}
        submitBehavior={returnKeyType === "next" ? "submit" : "blurAndSubmit"}
        onSubmitEditing={(event) => props.onSubmitEditing(event.nativeEvent.text)}
      />
      {secret ? (
        <View className="absolute right-1.5">
          <SecretRevealButton revealed={revealed} onPress={() => setRevealed(!revealed)} />
        </View>
      ) : null}
    </View>
  );
}
