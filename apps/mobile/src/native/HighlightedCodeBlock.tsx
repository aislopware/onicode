import { useState } from "react";
import { Pressable, ScrollView, Text } from "react-native";
import type { MarkdownStyle } from "react-native-enriched-markdown";

import { Menu } from "../components/ui/Menu";
import { useMarkdownCodeHighlight } from "../features/threads/markdownCodeHighlightState";
import { copyTextWithHaptic } from "../lib/copyTextWithHaptic";
import type { HighlightedLines, HighlightedToken, HighlightScheme } from "../lib/highlights";
import { CodeBlockFrame, CodeBlockHeaderButton } from "./CodeBlockFrame";
import { fencedCodeMarkdown } from "./codeBlockMarkdown";
import type { NativeMarkdownTextStyle } from "./SelectableMarkdownText.types";

// Header names from enriched-markdown's CodeBlockLanguages.cpp that are not the fence capitalized.
const FENCE_DISPLAY_NAMES: Readonly<Record<string, string>> = {
  cc: "C++",
  cpp: "C++",
  cxx: "C++",
  cs: "C#",
  csharp: "C#",
  graphql: "GraphQL",
  objc: "Objective-C",
  objectivec: "Objective-C",
  php: "PHP",
  rb: "Ruby",
  scss: "SCSS",
  sql: "SQL",
  toml: "TOML",
  xml: "XML",
};

/** Tokens keyed by their character offset, with `null` for each line break. */
function keyedTokens(lines: HighlightedLines) {
  const tokens: Array<{ readonly key: number; readonly token: HighlightedToken | null }> = [];
  let offset = 0;
  for (const [lineIndex, line] of lines.entries()) {
    if (lineIndex > 0) tokens.push({ key: offset++, token: null });
    for (const token of line) {
      tokens.push({ key: offset, token });
      offset += token.content.length;
    }
  }
  return tokens;
}

// The native block's long-press menu, with its labels and iOS symbols.
const CODE_MENU_ACTIONS = [
  { id: "copy-code", title: "Copy", image: "doc.on.doc" },
  { id: "copy-markdown", title: "Copy as Markdown", image: "doc.text" },
];

/** A fence in a language enriched-markdown cannot colour, drawn like its native block. */
export function HighlightedCodeBlock(props: {
  readonly code: string;
  readonly fence: string;
  readonly language: string;
  /** False only while the library withholds an unclosed fence (`streamingAnimation`, which Onicode never enables, so always true today); highlighting waits for it. */
  readonly complete: boolean;
  readonly scheme: HighlightScheme;
  readonly textStyle: NativeMarkdownTextStyle;
  readonly codeStyle: NonNullable<MarkdownStyle["codeBlock"]>;
}) {
  const { code, codeStyle, textStyle } = props;
  const [wrapped, setWrapped] = useState(false);
  const lines = useMarkdownCodeHighlight({
    code,
    language: props.language,
    theme: props.scheme,
    enabled: props.complete,
  });
  const fence = props.fence.toLowerCase();
  const text = (
    // Not selectable: as in the native block, a long-press opens the copy menu instead.
    <Text
      style={{
        color: codeStyle.color,
        fontFamily: codeStyle.fontFamily,
        fontSize: codeStyle.fontSize,
        lineHeight: codeStyle.lineHeight,
      }}
    >
      {lines
        ? keyedTokens(lines).map(({ key, token }) =>
            token === null ? (
              "\n"
            ) : (
              <Text key={key} style={token.color ? { color: token.color } : undefined}>
                {token.content}
              </Text>
            ),
          )
        : code}
    </Text>
  );
  // Inside the scroller: on iOS an ancestor JS responder disables a ScrollView's panning.
  const content = (
    <Menu
      shouldOpenOnLongPress
      actions={CODE_MENU_ACTIONS}
      style={{ flexGrow: 1 }}
      onPressAction={({ nativeEvent }) => {
        copyTextWithHaptic(
          nativeEvent.event === "copy-markdown" ? fencedCodeMarkdown(code, props.fence) : code,
        );
      }}
    >
      <Pressable style={{ padding: codeStyle.padding }}>{text}</Pressable>
    </Menu>
  );
  return (
    <CodeBlockFrame
      label={FENCE_DISPLAY_NAMES[fence] ?? fence.charAt(0).toUpperCase() + fence.slice(1)}
      textStyle={textStyle}
      actions={
        <>
          <CodeBlockHeaderButton
            icon="text.alignleft"
            label={wrapped ? "Disable line wrap" : "Wrap lines"}
            textStyle={textStyle}
            active={wrapped}
            onPress={() => setWrapped((value) => !value)}
          />
          <CodeBlockHeaderButton
            icon="doc.on.doc"
            label="Copy code"
            textStyle={textStyle}
            onPress={() => copyTextWithHaptic(code)}
          />
        </>
      }
    >
      {wrapped ? (
        content
      ) : (
        <ScrollView
          horizontal
          // As in the thread feed: with the keyboard open, the first long-press reaches the copy menu.
          keyboardShouldPersistTaps="always"
          contentContainerStyle={{ flexGrow: 1 }}
        >
          {content}
        </ScrollView>
      )}
    </CodeBlockFrame>
  );
}
