import { describe, expect, it } from "vite-plus/test";
import { ComposerContextId } from "@onicode/contracts";
import { formatComposerContextReference } from "@onicode/shared/composerContextReferences";

import { composerContextLinkMarkdown } from "./composerContextLinks";

const image = {
  kind: "image" as const,
  contextId: ComposerContextId.make("image-source"),
  label: "Checkout",
};
const terminal = {
  kind: "terminal" as const,
  contextId: ComposerContextId.make("terminal-source"),
  label: "Build",
};

describe("mobile context links", () => {
  it("renders references as links and keeps a ! or backslash before one as text", () => {
    const reference = formatComposerContextReference(image);
    const href = "oni-context://v1/image/image-source";
    const terminalReference = formatComposerContextReference(terminal);
    const terminalHref = "oni-context://v1/terminal/terminal-source";
    const cases: ReadonlyArray<[string, string]> = [
      [`See ${reference} now`, `See [Checkout](${href}) now`],
      [`Look!${reference}`, `Look&#33;[Checkout](${href})`],
      [`!${reference}`, `&#33;[Checkout](${href})`],
      [`Look\\!${reference}`, `Look\\![Checkout](${href})`],
      [`Look\\\\!${reference}`, `Look\\\\&#33;[Checkout](${href})`],
      [`Look\\${reference}`, `Look&#92;[Checkout](${href})`],
      [`C:\\dir\\${terminalReference}`, `C:\\dir&#92;[Checkout](${terminalHref})`],
      [`Look!${terminalReference}`, `Look&#33;[Checkout](${terminalHref})`],
      [`Look\\!${terminalReference}`, `Look\\![Checkout](${terminalHref})`],
    ];
    for (const [text, expected] of cases) {
      expect(composerContextLinkMarkdown(text, () => "Checkout")).toBe(expected);
    }
  });
});
