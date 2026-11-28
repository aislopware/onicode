import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import { mFallback, msg } from "gt-react";
import { expect, it } from "vite-plus/test";

import type { MessageResolver } from "~/i18n/messages";
import { PreviewError } from "./previewFailure";
import { previewError, previewFailureError } from "./previewFailureMessages";

it("keeps preview failures as errors the viewer words in the current language", () => {
  // Brackets show the text went through the render-time resolver.
  const resolve = ((message, options) => `[${mFallback(message, options)}]`) as MessageResolver;
  const fallback = msg("Unable to open PDF.");

  expect(
    thrownErrorMessage(previewFailureError({ code: "read-failed", message: "x" }), resolve),
  ).toBe("[Could not read this file.]");
  expect(thrownErrorMessage(previewFailureError({ message: "bad xref" }), resolve)).toBe(
    "bad xref",
  );
  expect(
    thrownErrorMessage(previewError(new PreviewError({ code: "parse-failed" }), fallback), resolve),
  ).toBe("[Could not parse this file.]");
  expect(thrownErrorMessage(previewError(new Error("socket closed"), fallback), resolve)).toBe(
    "socket closed",
  );
  expect(thrownErrorMessage(previewError(new Error(""), fallback), resolve)).toBe(
    "[Unable to open PDF.]",
  );
  expect(thrownErrorMessage(previewError("boom", fallback), resolve)).toBe("[Unable to open PDF.]");
});
