import { NodeViewContent, NodeViewWrapper, type NodeViewProps } from "@tiptap/react";

import { languageOfInfoString, withInfoStringLanguage } from "~/composer-code-languages";

import { useRenderedAppearance } from "../../hooks/useTheme";
import { MarkdownCodeBlockFrame } from "./MarkdownCodeBlock";
import { ComposerCodeBlockLanguagePicker } from "./ComposerCodeBlockLanguagePicker";

export function ComposerCodeBlockNodeView({
  node,
  editor,
  getPos,
  updateAttributes,
}: NodeViewProps) {
  const resolvedTheme = useRenderedAppearance();
  const info = typeof node.attrs.language === "string" ? node.attrs.language : "";
  const language = languageOfInfoString(info);

  const changeLanguage = (next: string) => {
    updateAttributes({ language: withInfoStringLanguage(info, next) });
    const position = getPos();
    if (typeof position === "number") {
      // Not the focus command: it defers DOM focus a frame, so keys typed meanwhile are lost.
      // Focus comes first because the view scrolls only to a selection it owns.
      editor.view.focus();
      editor
        .chain()
        .setTextSelection(position + 1 + node.content.size)
        .scrollIntoView()
        .run();
    }
  };

  return (
    <MarkdownCodeBlockFrame
      as={NodeViewWrapper}
      language={language || "text"}
      fenceTitle={null}
      theme={resolvedTheme}
      headerProps={{ contentEditable: false }}
      title={
        <ComposerCodeBlockLanguagePicker
          language={language}
          theme={resolvedTheme}
          disabled={!editor.isEditable}
          onChange={changeLanguage}
        />
      }
    >
      <div className="chat-markdown-shiki">
        <pre className="max-w-full overflow-x-auto px-3 pt-1 pb-2">
          <NodeViewContent<"code">
            as="code"
            className="block min-h-[1lh] font-mono whitespace-pre-wrap text-inherit [overflow-wrap:anywhere]"
          />
        </pre>
      </div>
    </MarkdownCodeBlockFrame>
  );
}
