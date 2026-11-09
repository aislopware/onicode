import { describe, expect, it } from "vite-plus/test";
import { codexAuthorizationRequest, codexCallbackUrl } from "./codexAuthRequest.ts";

const authorizationUrl = (() => {
  const url = new URL("https://auth.openai.com/api/accounts/authorize");
  url.search = new URLSearchParams({
    client_id: "dynamic_agent_client",
    response_type: "code",
    redirect_uri: "http://127.0.0.1:54213/auth/callback",
    state: "a".repeat(43),
    code_challenge_method: "S256",
    code_challenge: "b".repeat(43),
  }).toString();
  return url.toString();
})();
const callbackUrl = `http://127.0.0.1:54213/auth/callback?state=${"a".repeat(43)}&code=one-time-code&client_id=oaiapp_test`;

describe("Codex authorization request", () => {
  it("rejects other sites, duplicated parameters, and non-loopback callback addresses", () => {
    expect(() =>
      codexAuthorizationRequest(authorizationUrl.replace("auth.openai.com", "attacker.example")),
    ).toThrow();
    expect(() =>
      codexAuthorizationRequest(
        authorizationUrl + "&redirect_uri=http://localhost:1/auth/callback",
      ),
    ).toThrow();
    const url = new URL(authorizationUrl);
    url.searchParams.set("redirect_uri", "http://localhost:54213/auth/callback");
    expect(() => codexAuthorizationRequest(url.toString())).toThrow();
    url.searchParams.set("redirect_uri", "https://attacker.example/auth/callback");
    expect(() => codexAuthorizationRequest(url.toString())).toThrow();
  });
});

describe("pasted Codex callback", () => {
  const { redirectUri, state } = codexAuthorizationRequest(authorizationUrl);
  const copied = callbackUrl.slice("http://".length);

  it("accepts a callback copied without its scheme", () => {
    expect(codexCallbackUrl(copied, redirectUri, state).toString()).toBe(callbackUrl);
  });
  it("accepts leading whitespace with or without a scheme", () => {
    expect(codexCallbackUrl(` \t\n${callbackUrl}`, redirectUri, state).toString()).toBe(
      callbackUrl,
    );
    expect(codexCallbackUrl(` \t\n${copied}`, redirectUri, state).toString()).toBe(callbackUrl);
  });
  it("still rejects a scheme-less callback for another address or sign-in", () => {
    expect(() =>
      codexCallbackUrl(copied.replace("127.0.0.1", "localhost"), redirectUri, state),
    ).toThrow();
    expect(() => codexCallbackUrl(copied.replace("54213", "54214"), redirectUri, state)).toThrow();
    expect(() => codexCallbackUrl(`${copied}&state=another`, redirectUri, state)).toThrow();
  });
});
