import { TextInputWrapper } from "expo-paste-input";
import { useImperativeHandle, useRef } from "react";
import { TextInput, type TextInputInstance } from "react-native";

import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";
import { useScaledTextRole } from "../features/settings/appearance/useScaledTextRole";
import { useNativePaste } from "../lib/useNativePaste";
import type { ComposerEditorProps } from "./OniComposerEditor.types";

export function ComposerEditor({
  ref,
  skills: _skills,
  selection,
  onPasteImages,
  onPasteImagesRead: _onPasteImagesRead,
  onPasteText: _onPasteText,
  style,
  textStyle,
  contentInsetVertical = 0,
  singleLineCentered: _singleLineCentered,
  enterBehavior: _enterBehavior,
  readResetGeneration: _readResetGeneration,
  onRecallPrompt: _onRecallPrompt,
  readOnly = false,
  onContentSizeChange,
  ...props
}: ComposerEditorProps) {
  const inputRef = useRef<TextInputInstance>(null);
  const bodyText = useScaledTextRole("body");
  const fontFamily = useAppearancePreferences().fontFamilies.regular;
  const paste = useNativePaste(() => (uris) => {
    if (uris.length > 0) onPasteImages?.(uris);
  });

  useImperativeHandle(
    ref,
    () => ({
      focus: () => inputRef.current?.focus(),
      blur: () => inputRef.current?.blur(),
      setSelection: (nextSelection) =>
        inputRef.current?.setSelection(nextSelection.start, nextSelection.end),
      flushPendingInput: async () => true,
    }),
    [],
  );

  return (
    <TextInputWrapper {...paste} style={[{ minHeight: 0 }, style]}>
      <TextInput
        ref={inputRef}
        {...props}
        editable={(props.editable ?? true) && !readOnly}
        selection={selection}
        onSelectionChange={(event) => props.onSelectionChange?.(event.nativeEvent.selection)}
        onContentSizeChange={(event) => onContentSizeChange?.(event.nativeEvent.contentSize)}
        multiline={props.multiline ?? true}
        placeholderTextColorClassName={"accent-placeholder"}
        className="text-foreground"
        style={[
          {
            flex: 1,
            minHeight: 0,
            fontFamily,
            ...bodyText,
            paddingVertical: contentInsetVertical,
          },
          textStyle,
        ]}
      />
    </TextInputWrapper>
  );
}

export type {
  ComposerEditorHandle,
  ComposerEditorProps,
  ComposerEditorSelection,
  ComposerTextPaste,
} from "./OniComposerEditor.types";
