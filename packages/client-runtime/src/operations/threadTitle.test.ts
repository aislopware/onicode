import { mFallback } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";
import { describe, expect, it } from "vite-plus/test";

import { threadTitleText, threadTitleTextFromPrompt } from "./threadTitle.ts";

describe("threadTitleTextFromPrompt", () => {
  it("names a mentioned model by its slug, without the reference syntax", () => {
    expect(threadTitleTextFromPrompt("Delegate to model:codex/gpt-6.1-sol and report back")).toBe(
      "Delegate to gpt-6.1-sol and report back",
    );
    expect(
      threadTitleTextFromPrompt("model:opencode/anthropic/opus, then model:codex/gpt-5.4."),
    ).toBe("anthropic/opus, then gpt-5.4.");
  });
});

describe("threadTitleText", () => {
  it("shows the server's placeholder titles in the app's language", () => {
    // Brackets show the title went through the app's resolver.
    const translated = ((message, options) => `[${mFallback(message, options)}]`) as MFunctionType;
    expect(threadTitleText("New thread", translated)).toBe("[New thread]");
    expect(threadTitleText("Untitled thread", translated)).toBe("[Untitled thread]");
    expect(threadTitleText("  ", translated)).toBe("[Untitled thread]");
    expect(threadTitleText("Fix the build", translated)).toBe("Fix the build");
  });
});
