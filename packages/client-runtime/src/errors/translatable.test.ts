import { GitCommandError } from "@onicode/contracts";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import { mFallback } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";
import { describe, expect, it } from "vite-plus/test";

import { squashAtomCommandFailure } from "../state/runtime.ts";
import { errorDisplayMessage, thrownErrorMessage } from "./translatable.ts";

const decodeDefect = Schema.decodeUnknownSync(Schema.Defect());

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
    // A Git failure with no reason and no detail still says something.
    const blank = new GitCommandError({ operation: "push", command: "git", cwd: "/r", detail: "" });
    expect(thrownErrorMessage(blank, resolve)).toBe(blank.message);
    expect(errorDisplayMessage(blank, resolve)).toBeNull();
  });

  it("show the text of a string defect that came back over RPC", () => {
    const defect = decodeDefect(
      "RPC contract is registered but unavailable in the control plane: projectClone.start",
    );
    const error = squashAtomCommandFailure({ cause: Cause.die(defect) });

    expect(thrownErrorMessage(error)).toBe(
      "RPC contract is registered but unavailable in the control plane: projectClone.start",
    );
  });
});
