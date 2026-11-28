import { describe, expect, it } from "vite-plus/test";

import { acpUnnamedTextEndsInsideCode } from "./adapter.ts";

describe("acpUnnamedTextEndsInsideCode", () => {
  it("detects text ending inside an unclosed inline code span or code fence", () => {
    expect(
      acpUnnamedTextEndsInsideCode("Permissions (config `umask 027` app `0750` & keyring `0"),
    ).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("Use ``git status`` and ``npm test")).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("Look at `foo.")).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("Here is the script:\n```bash\necho 'done.'\n")).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("~~~\nls\n")).toBe(true);
    expect(
      acpUnnamedTextEndsInsideCode("- Steps:\n  - Run:\n\n    ```bash\n    echo a\n\n    echo b\n"),
    ).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("1. ```bash\n   echo a\n\n   echo b\n")).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("> ~~~\n> ls\n")).toBe(true);
    // Inside a fence, list and quote markers are code, not the end of the fence.
    expect(acpUnnamedTextEndsInsideCode("- Run:\n\n  ```md\n  - ```\n  still code\n")).toBe(true);
    expect(acpUnnamedTextEndsInsideCode("```md\n> ```\nstill code\n")).toBe(true);
  });

  it("treats closed code and plain text as safe to split", () => {
    expect(acpUnnamedTextEndsInsideCode("Let me check `config.json`.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("Use \\`escaped\\` ticks.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("Here is the script:\n```bash\necho ok\n```\n")).toBe(
      false,
    );
    expect(acpUnnamedTextEndsInsideCode("- Steps:\n\n    ```bash\n    echo a\n\n    ```\n")).toBe(
      false,
    );
    expect(acpUnnamedTextEndsInsideCode("> ~~~\n> ls\n> ~~~\n")).toBe(false);
    // Indented code outside a list is not a fence, and a fence ends with its quote or list item.
    expect(acpUnnamedTextEndsInsideCode("Example:\n\n    ~~~\n    code\n\nDone.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("> ~~~\n> ls\n\nOutside the quote.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("- ~~~\n  ls\nOutside the list.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("I'll search **/*.ts for usages.")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("Checking")).toBe(false);
    expect(acpUnnamedTextEndsInsideCode("")).toBe(false);
  });
});
