import { useState } from "react";
import { View } from "react-native";

import { AppTextInput } from "./AppText";
import { SecretRevealButton } from "./SecretRevealButton";
import type { SecretTextInputProps } from "./SecretTextInput.types";

/** Masked field a password manager can fill; keyboard suggestions stay off. */
export function SecretTextInput(props: SecretTextInputProps) {
  const [revealed, setRevealed] = useState(false);
  return (
    <View className="justify-center">
      <AppTextInput
        className="pr-12"
        accessibilityLabel={props.accessibilityLabel}
        placeholder={props.placeholder}
        value={props.value}
        onChangeText={props.onChangeText}
        editable={props.editable}
        secureTextEntry={!revealed}
        autoCorrect={false}
        autoCapitalize="none"
        // Puts the Passwords key above the keyboard, which opens the person's password manager.
        autoComplete="current-password"
        textContentType="password"
        spellCheck={false}
        returnKeyType="done"
        onSubmitEditing={(event) => props.onSubmitEditing(event.nativeEvent.text)}
      />
      <View className="absolute right-1.5">
        <SecretRevealButton revealed={revealed} onPress={() => setRevealed(!revealed)} />
      </View>
    </View>
  );
}
