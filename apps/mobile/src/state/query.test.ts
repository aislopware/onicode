import { translatableError } from "@onicode/client-runtime/errors";
import * as Cause from "effect/Cause";
import { mFallback, msg } from "gt-react-native";
import { describe, expect, it } from "vite-plus/test";

import type { MessageResolver } from "../i18n/messages";
import { formatEnvironmentQueryError } from "./query";

describe("formatEnvironmentQueryError", () => {
  it("shows an error's marked text through the given resolver", () => {
    // Brackets show the text went through the resolver, so it follows a language switch.
    const resolve = ((message, options) => `[${mFallback(message, options)}]`) as MessageResolver;

    expect(
      formatEnvironmentQueryError(
        Cause.fail(translatableError(msg("Reconnect to this environment and try again."))),
        resolve,
      ),
    ).toBe("[Reconnect to this environment and try again.]");
    expect(formatEnvironmentQueryError(Cause.fail(new Error("boom")), resolve)).toBe("boom");
    expect(formatEnvironmentQueryError(Cause.fail(new Error(" ")), resolve)).toBe(
      "[The environment request failed.]",
    );
  });
});
