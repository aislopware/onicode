import {
  AuthMcpApprovalError,
  AuthMcpAuthorizationRequest,
  AuthMcpClientRegistration,
  AuthMcpRegistrationError,
  EnvironmentHttpApi,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";
import * as Schema from "effect/Schema";
import { HttpServerRequest, HttpServerResponse } from "effect/http";
import * as HttpEffect from "effect/http/HttpEffect";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";

import * as EnvironmentAuth from "./EnvironmentAuth.ts";
import * as McpOAuth from "./McpOAuth.ts";
import {
  consentPath,
  PAGE_HEADERS,
  renderCodePage,
  renderErrorPage,
  renderWaitingPage,
} from "./mcpOAuthHtml.ts";

// OAuth responses contain credentials and must not be cached.
const noStore = HttpEffect.appendPreResponseHandler((_request, response) =>
  Effect.succeed(
    HttpServerResponse.setHeaders(response, { "cache-control": "no-store", pragma: "no-cache" }),
  ),
);

const requestUrls = Effect.map(HttpServerRequest.HttpServerRequest, McpOAuth.requestUrls);
const decodeAuthorizationRequest = Schema.decodeUnknownEffect(AuthMcpAuthorizationRequest);
// Decoded here so malformed metadata gets the RFC 7591 error, not HttpApi's empty 400.
const decodeClientRegistration = Schema.decodeUnknownEffect(
  Schema.fromJsonString(AuthMcpClientRegistration),
);
const ConsentForm = Schema.Struct({ id: Schema.String, code: Schema.String });
const decodeConsentForm = Schema.decodeUnknownEffect(ConsentForm);

const html = (body: string, status = 200) =>
  HttpServerResponse.text(body, {
    status,
    contentType: "text/html; charset=utf-8",
    headers: PAGE_HEADERS,
  });

const redirect = (location: string, status: 302 | 303 = 302) =>
  HttpServerResponse.redirect(location, {
    status,
    headers: { "cache-control": "no-store", "referrer-policy": "no-referrer" },
  });

const queryParam = (request: HttpServerRequest.HttpServerRequest, name: string) =>
  new URL(request.url, "http://localhost").searchParams.get(name) ?? "";

const pageFor = (page: McpOAuth.SignInPage, render: (view: McpOAuth.SignInView) => string) =>
  page._tag === "finished"
    ? redirect(page.redirectTo)
    : page._tag === "waiting"
      ? html(render(page.signIn))
      : html(renderErrorPage("This sign-in finished or expired."), 404);

/** Back through the consent page: CSP's form-action would stop a redirect off-origin. */
const afterCode = (id: string, page: McpOAuth.SignInPage) =>
  page._tag === "missing"
    ? html(renderErrorPage("This sign-in finished or expired."), 404)
    : redirect(consentPath(id), 303);

export const layer = HttpApiBuilder.group(
  EnvironmentHttpApi,
  "mcpOAuth",
  Effect.fnUntraced(function* (handlers) {
    const oauth = yield* McpOAuth.McpOAuth;
    const environmentAuth = yield* EnvironmentAuth.EnvironmentAuth;
    const protectedResource = () =>
      requestUrls.pipe(Effect.map(McpOAuth.protectedResourceMetadata));

    return handlers
      .handle("protectedResource", protectedResource)
      .handle("mcpProtectedResource", protectedResource)
      .handle("authorizationServer", () =>
        requestUrls.pipe(Effect.map(McpOAuth.authorizationServerMetadata)),
      )
      .handleRaw("register", ({ request }) =>
        request.text.pipe(
          Effect.orDie,
          Effect.flatMap(decodeClientRegistration),
          Effect.mapError(
            (cause) =>
              new AuthMcpRegistrationError({
                error: "invalid_client_metadata",
                error_description: `Invalid client metadata: ${cause.message}`,
              }),
          ),
          Effect.flatMap(oauth.register),
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
      .handleRaw("authorize", ({ request }) =>
        Effect.gen(function* () {
          const urls = yield* requestUrls;
          const url = new URL(request.url, urls.issuer);
          const authorizationRequest = yield* decodeAuthorizationRequest(
            Object.fromEntries(url.searchParams),
          ).pipe(Effect.orDie);
          const authorization = yield* oauth
            .validateAuthorization({ urls, request: authorizationRequest })
            .pipe(Effect.result);
          if (authorization._tag === "Failure") {
            const error = authorization.failure;
            // An unverified client or redirect is reported, never redirected.
            return error._tag === "McpOAuthPageError"
              ? html(renderErrorPage(error.description), 400)
              : redirect(McpOAuth.redirectForError(error, urls.issuer));
          }
          if (authorization.success.client.peerLink) {
            return html(
              renderErrorPage("Links between environments are made from the other environment."),
              400,
            );
          }
          return yield* oauth.beginSignIn(authorization.success).pipe(
            Effect.map((signIn) => redirect(consentPath(signIn.id), 303)),
            Effect.catchTags({
              McpOAuthPageError: (error) =>
                Effect.succeed(html(renderErrorPage(error.description), error.busy ? 429 : 403)),
            }),
          );
        }),
      )
      .handleRaw("consent", ({ request }) =>
        oauth
          .signInPage(queryParam(request, "id"))
          .pipe(Effect.map((page) => pageFor(page, renderWaitingPage))),
      )
      .handleRaw("consentCodeForm", ({ request }) =>
        oauth
          .signInPage(queryParam(request, "id"))
          .pipe(Effect.map((page) => pageFor(page, (view) => renderCodePage(view)))),
      )
      .handleRaw("consentCode", ({ request }) =>
        Effect.gen(function* () {
          const form = yield* request.urlParamsBody.pipe(
            Effect.flatMap((params) => decodeConsentForm(Object.fromEntries(params))),
            Effect.option,
          );
          if (form._tag === "None") return html(renderErrorPage("The form was incomplete."), 400);
          const { id, code } = form.value;
          return yield* oauth.approveSignInWithCode({ id, code }).pipe(
            Effect.map((page) => afterCode(id, page)),
            Effect.catchTags({
              McpOAuthPageError: (error) =>
                oauth.peekSignIn(id).pipe(
                  Effect.map((page) =>
                    page._tag !== "waiting"
                      ? afterCode(id, page)
                      : error.busy
                        ? // A repeated submit: the waiting page refreshes into the first one's result.
                          html(renderWaitingPage(page.signIn))
                        : html(renderCodePage(page.signIn, error.description), 400),
                  ),
                ),
            }),
          );
        }),
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
      )
      .handle("revoke", ({ payload }) =>
        Effect.gen(function* () {
          yield* noStore;
          // RFC 7009 §2.2: an unknown or already revoked token gets the same 200 as a revoked one.
          if (payload.token === undefined) return;
          yield* environmentAuth
            .revokeMcpClientToken(Redacted.value(payload.token))
            .pipe(Effect.orDie);
        }),
      );
  }),
);
