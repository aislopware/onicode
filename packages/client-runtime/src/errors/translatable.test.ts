import { GitCommandError } from "@onicode/contracts";
import { mFallback } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";
import { describe, expect, it } from "vite-plus/test";

import { errorDisplayMessage, thrownErrorMessage } from "./translatable.ts";

describe("error display messages", () => {
  it("show a Git failure in the active language, without the operation and cwd", () => {
    const resolve = ((message, options) => `[fr] ${mFallback(message, options)}`) as MFunctionType;
    const failure = (reason?: "branch_already_exists") =>
      new GitCommandError({
        operation: "GitVcsDriver.createRef",
        command: "git",
        cwd: "/tmp/repo",
        detail: "Commit, stash, or discard them, then switch again.",
        ...(reason ? { reason } : {}),
      });

    for (const display of [thrownErrorMessage, errorDisplayMessage]) {
      expect(display(failure("branch_already_exists"), resolve)).toBe(
        "[fr] A branch with this name already exists.",
      );
      expect(display(failure(), resolve)).toBe(
        "Commit, stash, or discard them, then switch again.",
      );
      expect(display(new Error("boom"), resolve)).toBe("boom");
    }
  });
});
