import { describe, expect, it } from "vite-plus/test";
import { providerAuthReturnUrl } from "./providerAuthReturnUrl.ts";

describe("provider auth return destinations", () => {
  it.each(["onicode://app/", "onicode-dev://app/"])("returns to %s", (url) =>
    expect(providerAuthReturnUrl(url)).toBe(url),
  );
  it.each([
    "onicode://attacker/",
    "onicode://user@app/",
    "onicode://app/#/settings/providers?code=secret",
    "onicode-dev://app/auth/callback?code=secret",
    "https://attacker.example/",
    "http://localhost:3773/",
  ])("rejects %s", (url) => expect(providerAuthReturnUrl(url)).toBeUndefined());
});
