import { AuthMcpApprovalError, EnvironmentHttpApi } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import { HttpServerRequest, HttpServerResponse } from "effect/http";
import * as HttpEffect from "effect/http/HttpEffect";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";

import * as McpOAuth from "./McpOAuth.ts";

// OAuth responses contain credentials and must not be cached.
const noStore = HttpEffect.appendPreResponseHandler((_request, response) =>
  Effect.succeed(
    HttpServerResponse.setHeaders(response, { "cache-control": "no-store", pragma: "no-cache" }),
  ),
);

const requestUrls = Effect.map(HttpServerRequest.HttpServerRequest, McpOAuth.requestUrls);

export const layer = HttpApiBuilder.group(
  EnvironmentHttpApi,
  "mcpOAuth",
  Effect.fnUntraced(function* (handlers) {
    const oauth = yield* McpOAuth.McpOAuth;

    return handlers
      .handle("register", ({ payload }) =>
        oauth.register(payload).pipe(
          Effect.map((client) => ({
            client_id: client.clientId,
            client_name: client.name,
            redirect_uris: client.redirectUris,
            grant_types: ["authorization_code" as const],
            response_types: ["code" as const],
            token_endpoint_auth_method: "none" as const,
          })),
          Effect.tap(() => noStore),
        ),
      )
      .handle("decision", ({ payload }) =>
        Effect.gen(function* () {
          const urls = yield* requestUrls;
          yield* noStore;
          // An unverified client or redirect is reported, never redirected; any other problem
          // goes back through the (verified) redirect URI.
          const authorization = yield* oauth
            .validateAuthorization({ urls, request: payload.authorization })
            .pipe(
              Effect.catchTags({
                McpOAuthPageError: (error) =>
                  Effect.fail(new AuthMcpApprovalError({ message: error.description })),
                McpOAuthRedirectError: (error) =>
                  Effect.succeed({ redirectTo: McpOAuth.redirectForError(error, urls.issuer) }),
              }),
            );
          if ("redirectTo" in authorization) return authorization;
          return yield* oauth.approve({ authorization, decision: payload.decision }).pipe(
            Effect.map((redirectTo) => ({ redirectTo })),
            Effect.mapError((error) => new AuthMcpApprovalError({ message: error.message })),
          );
        }),
      )
      .handle("token", ({ payload, request }) =>
        Effect.gen(function* () {
          yield* noStore;
          return yield* oauth.exchangeCode({ request, urls: yield* requestUrls, token: payload });
        }),
      );
  }),
);
