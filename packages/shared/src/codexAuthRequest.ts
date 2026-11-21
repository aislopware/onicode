import { oauthCallbackUrl } from "./oauthCallback.ts";

export function codexAuthorizationRequest(value: string) {
  const url = new URL(value);
  if (
    value.length > 16_384 ||
    url.origin !== "https://auth.openai.com" ||
    url.pathname !== "/api/accounts/authorize" ||
    url.username ||
    url.password ||
    url.hash
  )
    throw new Error("Invalid ChatGPT sign-in request.");
  const single = (key: string) => {
    const values = url.searchParams.getAll(key);
    if (values.length !== 1 || !values[0]) throw new Error("Invalid ChatGPT sign-in request.");
    return values[0];
  };
  const redirectUri = single("redirect_uri");
  const redirect = new URL(redirectUri);
  const state = single("state");
  if (
    !/^http:\/\/127\.0\.0\.1:[1-9]\d{0,4}\/auth\/callback$/u.test(redirectUri) ||
    Number(redirect.port) > 65_535 ||
    !/^[\w-]{16,128}$/u.test(state) ||
    single("response_type") !== "code" ||
    single("code_challenge_method") !== "S256" ||
    !/^[\w-]{43}$/u.test(single("code_challenge")) ||
    !/^(dynamic_agent_client|oaiapp_[\w-]+)$/u.test(single("client_id"))
  )
    throw new Error("Invalid ChatGPT sign-in request.");
  return { authorizationUrl: url.toString(), redirectUri, state };
}

export function codexCallbackUrl(value: string, redirectUri: string, state: string) {
  const callback = oauthCallbackUrl(value, redirectUri, state);
  const clients = callback.searchParams.getAll("client_id");
  if (clients.length > 1 || (clients.length === 1 && !/^oaiapp_[\w-]+$/u.test(clients[0]!)))
    throw new Error("This redirect URL does not belong to the current sign-in.");
  return callback;
}
