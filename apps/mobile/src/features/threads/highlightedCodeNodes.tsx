import type { ReactNode } from "react";

import { AppText as Text } from "../../components/ui/AppText";
import type { HighlightedLines } from "../../lib/highlights";

/** Highlighted lines as nested text runs, keyed by each token's character offset. */
export function highlightedCodeNodes(lines: HighlightedLines): ReactNode[] {
  let offset = 0;
  return lines.flatMap((line, lineIndex) => {
    const separator = lineIndex > 0 ? "\n" : "";
    offset += separator.length;
    return [
      separator,
      ...line.map((token) => {
        const start = offset;
        offset += token.content.length;
        return (
          <Text key={start} style={token.color ? { color: token.color } : undefined}>
            {token.content}
          </Text>
        );
      }),
    ];
  });
}
