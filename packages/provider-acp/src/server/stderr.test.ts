import { describe, expect, it } from "vite-plus/test";

import { sanitizeAcpStderrExcerpt } from "./stderr.ts";

describe("AcpStderr", () => {
  it("redacts home paths, pairing URLs, and tokens from stderr excerpts", () => {
    const excerpt = sanitizeAcpStderrExcerpt(
      [
        "Invalid project config at /Users/ada/.cursor/cli.json",
        "Authorization: Bearer secret-token-value",
        "Sign in: https://accounts.google.com/o/oauth2/v2/auth?state=oauth-state-value",
        "Visit http://localhost:5733/pair#token=ABCDEF for pairing",
        "key=sk-abcdefghijklmnopqrstuv",
      ].join("\n"),
      "/home/server",
      { HOME: "/Users/ada" },
    );

    expect(excerpt).toContain("Invalid project config at ~/.cursor/cli.json");
    expect(excerpt).toContain("Bearer [redacted]");
    expect(excerpt).toContain("[pairing-url]");
    expect(excerpt).toContain("[redacted]");
    expect(excerpt).not.toContain("secret-token-value");
    expect(excerpt).not.toContain("ABCDEF");
    expect(excerpt).toContain("[sign-in-url]");
    expect(excerpt).not.toContain("oauth-state-value");
    expect(excerpt).not.toContain("sk-abcdefghijklmnopqrstuv");

    const ambientHome = process.env.HOME ?? process.env.USERPROFILE;
    if (
      ambientHome !== undefined &&
      ambientHome.length > 1 &&
      !ambientHome.includes("/home/onicode-test-home")
    ) {
      expect(
        sanitizeAcpStderrExcerpt(
          `Config at ${ambientHome}/.config and /home/onicode-test-home/.config`,
          "/home/onicode-test-home",
        ),
      ).toBe(`Config at ${ambientHome}/.config and ~/.config`);
    }
  });

  it("redacts hyphenated OpenAI project keys and header credentials", () => {
    const excerpt = sanitizeAcpStderrExcerpt(
      [
        "openai=sk-proj-abcdefghijklmnopqrstuvwxyz012345",
        "svc=sk-svcacct-abcdefghijklmnopqrstuvwxyz012345",
        "anthropic=sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345",
        "Authorization: Basic dXNlcjpwYXNz",
        "x-api-key: ant-api-key-value",
      ].join("\n"),
      "/Users/ada",
      {},
    );

    expect(excerpt).toContain("[redacted]");
    expect(excerpt).toContain("Authorization: Basic [redacted]");
    expect(excerpt).toContain("x-api-key: [redacted]");
    expect(excerpt).not.toContain("sk-proj-");
    expect(excerpt).not.toContain("sk-svcacct-");
    expect(excerpt).not.toContain("sk-ant-api03-");
    expect(excerpt).not.toContain("abcdefghijklmnopqrstuvwxyz012345");
    expect(excerpt).not.toContain("dXNlcjpwYXNz");
    expect(excerpt).not.toContain("ant-api-key-value");
  });

  it("redacts the whole key when its body contains underscores", () => {
    const excerpt = sanitizeAcpStderrExcerpt(
      [
        "anthropic=sk-ant-api03-AbCdEf_GhIjKlMnOpQr-StUvWx_yz0123456789-AA",
        "openai=sk-proj-AbCdEfGhIj_KlMnOpQrStUv-WxYz_0123456789",
      ].join("\n"),
      "/Users/ada",
      {},
    );

    expect(excerpt).toBe("anthropic=[redacted]\nopenai=[redacted]");
  });

  it.each([
    {
      name: "Node stack trace extracts the actionable error line",
      stderr: [
        "file:///Users/ada/cli/index.js:107",
        "  throw new Error(",
        "        ^",
        "",
        "Error: Missing optional dependency @antigravity/darwin-arm64",
        "    at findExecutable (file:///Users/ada/cli/index.js:107:9)",
        "",
        "Node.js v26.8.2",
      ].join("\n"),
      excerpt: "Error: Missing optional dependency @antigravity/darwin-arm64",
    },
    {
      name: "Node error with error code",
      stderr:
        "node:internal/modules/esm/resolve:873\nError [ERR_MODULE_NOT_FOUND]: Cannot find package 'x'\n    at packageResolve\n",
      excerpt: "Error [ERR_MODULE_NOT_FOUND]: Cannot find package 'x'",
    },
    {
      name: "Rust CLI error before usage block",
      stderr: "error: unexpected argument '--bad' found\n\nUsage: agy [OPTIONS]\n",
      excerpt: "error: unexpected argument '--bad' found",
    },
    {
      name: "glog fatal line before stack trace",
      stderr: [
        "I1123 17:36:19.157122 26 server.go:10] Starting server",
        "F1111 11:50:12.383176 page-size.cc:19] Check failed: Compile-time page size does not match",
        "*** Check failure stack trace: ***",
        "@ 0x10293847",
      ].join("\n"),
      excerpt:
        "F1111 11:50:12.383176 page-size.cc:19] Check failed: Compile-time page size does not match",
    },
    {
      name: "launcher exit summary includes preceding failure lines",
      stderr: [
        "Starting provider",
        'oauth2: "invalid_request" "RefreshTokenInvalid"',
        "Error: provider launcher exited with code 1.",
      ].join("\n"),
      excerpt:
        'Starting provider | oauth2: "invalid_request" "RefreshTokenInvalid" | Error: provider launcher exited with code 1.',
    },
    {
      name: "ANSI and C1 terminal controls with whitespace normalization and redaction",
      stderr:
        "\u001b[31m  Error:\t  /Users/ada/config   Authorization: \u001b[1mBearer secret-val\u001b[0m \r\n\u009b0mUsage: agy\n",
      excerpt: "Error: ~/config Authorization: Bearer [redacted]",
    },
    {
      name: "C1 control sequence before error line",
      stderr: "\u009b31mError: invalid config\u009b0m\nUsage: agy [OPTIONS]\n",
      excerpt: "Error: invalid config",
    },
    {
      name: "blank and VT-only stderr returns empty string",
      stderr: "\n \t\r\n\u001b[0m\n",
      excerpt: "",
    },
    {
      name: "oversized error line is head-bounded to 512 chars",
      stderr: `Error: ${"x".repeat(1_000)}`,
      excerpt: `Error: ${"x".repeat(504)}…`,
    },
    {
      name: "oversized unlabelled stderr is tail-bounded to 512 chars",
      stderr: `${"a".repeat(600)}\nfinal detail line\n`,
      excerpt: `…${`${"a".repeat(600)}\nfinal detail line`.slice(-511)}`,
    },
  ])("strips VT controls and extracts actionable stderr: $name", ({ stderr, excerpt }) => {
    const result = sanitizeAcpStderrExcerpt(stderr, "/home/server", { HOME: "/Users/ada" });
    expect(result).toBe(excerpt);
    expect(result.length).toBeLessThanOrEqual(512);
  });
});
