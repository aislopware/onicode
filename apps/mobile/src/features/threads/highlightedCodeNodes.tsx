import type { ReactNode } from "react";
import { Text } from "react-native";

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
        // Plain RN Text inherits the parent's mono font; AppText would reset it to sans.
        return (
          <Text key={start} style={token.color ? { color: token.color } : undefined}>
            {token.content}
          </Text>
        );
      }),
    ];
  });
}
