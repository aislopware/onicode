import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import {
  AuthMcpAuthorizationRequest,
  AuthMcpClientAccess,
  EnvironmentHttpApi,
  MCP_PEER_LINK_SOFTWARE_ID,
  mcpApprovalCode,
  type ManagedPolicyDocument,
  mcpClientScopes,
} from "@onicode/contracts";
import { expect, it } from "@effect/vitest";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Etag from "effect/http/Etag";
import * as HttpPlatform from "effect/http/HttpPlatform";
import * as HttpRouter from "effect/http/HttpRouter";
import * as HttpServerRequest from "effect/http/HttpServerRequest";
import * as HttpApi from "effect/http-api/HttpApi";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";

import * as ServerConfig from "../config.ts";
import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ServerHttp from "../http.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as Sqlite from "../persistence/Sqlite.ts";
import * as EnvironmentAuth from "./EnvironmentAuth.ts";
import * as McpOAuth from "./McpOAuth.ts";
import * as McpOAuthHttp from "./mcpOAuthHttp.ts";
import * as ServerSecretStore from "./ServerSecretStore.ts";
import * as AuthHttp from "./http.ts";
import * as TailnetAccess from "../tailnet/TailnetAccess.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";

class AuthTestApi extends HttpApi.make("environment")
  .add(EnvironmentHttpApi.groups.auth)
  .add(EnvironmentHttpApi.groups.mcpOAuth) {}

const layerConfig = ServerConfig.layerTest(process.cwd(), { prefix: "oni-mcp-oauth-test-" });
const layerEnvironmentAuth = (policy?: ManagedPolicyDocument) =>
  EnvironmentAuth.layer.pipe(
    Layer.provide(ManagedPolicy.layerTest(policy)),
    Layer.provide(Sqlite.layerMemory),
    Layer.provideMerge(ServerSecretStore.layer),
    Layer.provideMerge(ServerEnvironment.layerIdentity),
    Layer.provide(layerConfig),
  );
interface Captured {
  readonly auth: EnvironmentAuth.EnvironmentAuth["Service"];
  readonly oauth: McpOAuth.McpOAuth["Service"];
}

/** Holds McpOAuth's first approval-code check until `release`, so a test can act while it runs. */
interface ApprovalCodeGate {
  readonly entered: Deferred.Deferred<void>;
  readonly release: Deferred.Deferred<void>;
  readonly checks: Ref.Ref<number>;
}
const makeApprovalCodeGate = Effect.all({
  entered: Deferred.make<void>(),
  release: Deferred.make<void>(),
  checks: Ref.make(0),
});
const layerApprovalCodeGate = (gate: ApprovalCodeGate | undefined) =>
  Layer.effect(
    EnvironmentAuth.EnvironmentAuth,
    Effect.map(EnvironmentAuth.EnvironmentAuth, (auth) =>
      gate === undefined
        ? auth
        : EnvironmentAuth.EnvironmentAuth.of({
            ...auth,
            consumeMcpApprovalCode: (code, access, purpose) =>
              Ref.getAndUpdate(gate.checks, (checks) => checks + 1).pipe(
                Effect.flatMap((earlier) =>
                  earlier === 0
                    ? Deferred.succeed(gate.entered, undefined).pipe(
                        Effect.andThen(Deferred.await(gate.release)),
                      )
                    : Effect.void,
                ),
                Effect.andThen(auth.consumeMcpApprovalCode(code, access, purpose)),
              ),
          }),
    ),
  );

interface RouteOptions {
  readonly policy?: ManagedPolicyDocument;
  readonly gate?: ApprovalCodeGate;
}

const makeLayerRoutes = (
  capture: (captured: Captured) => void,
  { policy, gate }: RouteOptions = {},
) =>
  Layer.mergeAll(
    Layer.effectDiscard(
      Effect.all({ auth: EnvironmentAuth.EnvironmentAuth, oauth: McpOAuth.McpOAuth }).pipe(
        Effect.tap((captured) => Effect.sync(() => capture(captured))),
      ),
    ),
    HttpApiBuilder.layer(AuthTestApi).pipe(
      Layer.provide(AuthHttp.layer),
      Layer.provide(TailnetAccess.layerUnavailable),
      Layer.provide(McpOAuthHttp.layer),
      Layer.provide(AuthHttp.layerAuthenticatedAuth),
      Layer.provide(ServerHttp.layerApiBodyLimit),
    ),
  ).pipe(
    Layer.provideMerge(
      McpOAuth.layer.pipe(
        Layer.provide(ServerSettings.layerTest({ linkedEnvironmentsEnabled: true })),
        Layer.provide(layerApprovalCodeGate(gate)),
      ),
    ),
    Layer.provideMerge(layerEnvironmentAuth(policy)),
    Layer.provide(layerConfig),
    Layer.provideMerge(
      HttpPlatform.layer.pipe(
        Layer.provideMerge(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
        Layer.provideMerge(Etag.layerWeak),
      ),
    ),
    Layer.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
  );

const encodeJson = Schema.encodeSync(Schema.fromJsonString(Schema.Unknown));
const ORIGIN = "https://box.example.ts.net";
const decodeAuthorizationRequest = Schema.decodeEffect(AuthMcpAuthorizationRequest);
const REDIRECT = "http://localhost/callback";
const verifier = "a".repeat(43) + "-verifier-for-tests";
const challenge = "DeB41nTVkPwpbbYecrnqtVq7VXLezustdHAK4SWt13c";

const at = (path: string, init?: RequestInit) =>
  new Request(`http://127.0.0.1${path}`, {
    ...init,
    headers: { host: "box.example.ts.net", "x-forwarded-proto": "https", ...init?.headers },
  });
const form = (body: Record<string, string>) => ({
  method: "POST",
  headers: { "content-type": "application/x-www-form-urlencoded" },
  body: new URLSearchParams(body).toString(),
});

type Handler = (request: Request) => Effect.Effect<Response>;

const withRoutes = <A, E>(
  use: (
    handler: Handler,
    auth: EnvironmentAuth.EnvironmentAuth["Service"],
    oauth: McpOAuth.McpOAuth["Service"],
  ) => Effect.Effect<A, E>,
  options?: RouteOptions,
) =>
  Effect.gen(function* () {
    const random = yield* SecureRandom.SecureRandom;
    const context = Context.make(SecureRandom.SecureRandom, random);
    let captured: Captured | undefined;
    const layerRoutes = makeLayerRoutes((services) => {
      captured = services;
    }, options);
    return yield* Effect.acquireUseRelease(
      Effect.sync(() => HttpRouter.toWebHandler(layerRoutes, { disableLogger: true })),
      (web) =>
        Effect.gen(function* () {
          const handler: Handler = (request) => Effect.promise(() => web.handler(request, context));
          yield* handler(at("/api/auth/session"));
          return yield* use(handler, captured!.auth, captured!.oauth);
        }),
      (web) => Effect.promise(() => web.dispose()),
    );
  }).pipe(Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)));

const json = <A>(response: Response) => Effect.promise(() => response.json() as Promise<A>);

const postJson = (path: string, body: unknown) =>
  at(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: encodeJson(body),
  });

const decide = (
  handler: Handler,
  authorization: Record<string, string>,
  decision: Record<string, string>,
) =>
  handler(postJson("/oauth/mcp/decision", { authorization, decision })).pipe(
    Effect.flatMap((response) =>
      json<{ redirectTo?: string; message?: string }>(response).pipe(
        Effect.map((payload) => ({ status: response.status, ...payload })),
      ),
    ),
  );

const register = (
  handler: Handler,
  redirect = REDIRECT,
  softwareId: string | null = MCP_PEER_LINK_SOFTWARE_ID,
) =>
  handler(
    at("/oauth/mcp/register", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: encodeJson({
        client_name: "Workstation",
        redirect_uris: [redirect],
        ...(softwareId === null ? {} : { software_id: softwareId }),
        token_endpoint_auth_method: "none",
      }),
    }),
  );

const registeredClientId = (handler: Handler) =>
  register(handler).pipe(
    Effect.flatMap((response) => json<{ client_id: string }>(response)),
    Effect.map((body) => body.client_id),
  );

const authorizeParams = (clientId: string, redirect = "http://localhost:51234/callback") => ({
  response_type: "code",
  client_id: clientId,
  redirect_uri: redirect,
  code_challenge: challenge,
  code_challenge_method: "S256",
  state: "state-1",
  resource: `${ORIGIN}/mcp`,
});

it.effect("answers 413 to a body over the API limit", () =>
  withRoutes((handler) =>
    Effect.gen(function* () {
      const body = "x".repeat(ServerHttp.API_MAX_BODY_BYTES + 1);
      for (const [path, contentType] of [
        ["/oauth/token", "application/x-www-form-urlencoded"],
        ["/oauth/mcp/register", "application/json"],
      ]) {
        const init = { method: "POST", headers: { "content-type": contentType! } };
        expect((yield* handler(at(path!, { ...init, body }))).status, path).toBe(413);
        // A chunked body declares no length, so the reader itself has to stop.
        const chunked = new Blob([body]).stream();
        const streamed = at(path!, { ...init, body: chunked, duplex: "half" } as RequestInit);
        expect((yield* handler(streamed)).status, `${path} chunked`).toBe(413);
      }
    }),
  ),
);

it.effect("registers any client and never redirects for an unverified one", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      for (const refused of [
        "http://bot.example/callback",
        "https://user:pass@bot.example/callback",
        "https://bot.example/callback#",
        "http://localhost/callback#",
        "javascript:alert(1)",
      ]) {
        const response = yield* register(handler, refused);
        expect(response.status).toBe(400);
        expect(yield* json<unknown>(response)).toMatchObject({ error: "invalid_redirect_uri" });
      }
      for (const softwareId of [null, "claude-code"]) {
        expect((yield* register(handler, REDIRECT, softwareId)).status).toBe(201);
      }

      const registered = yield* register(handler);
      expect(registered.status).toBe(201);
      const { client_id: clientId } = (yield* json<unknown>(registered)) as { client_id: string };
      const pairing = yield* auth.issuePairingCredential();
      const decision = { _tag: "pairing-code", access: "auto", code: pairing.credential };

      const forged = yield* decide(
        handler,
        authorizeParams(`${clientId.split(".")[0]}.forged`),
        decision,
      );
      expect(forged.status).toBe(400);
      expect(forged.redirectTo).toBeUndefined();

      const unregistered = yield* decide(
        handler,
        authorizeParams(clientId, "http://localhost:51234/elsewhere"),
        decision,
      );
      expect(unregistered.status).toBe(400);
      expect(unregistered.redirectTo).toBeUndefined();

      // A bad request redirects back only to a loopback app; anyone can register an https one.
      const authorize = (params: Record<string, string>) =>
        handler(at(`/oauth/mcp/authorize?${new URLSearchParams(params)}`));
      const unsupported = yield* authorize({
        ...authorizeParams(clientId),
        response_type: "token",
      });
      expect(unsupported.status).toBe(302);
      expect(new URL(unsupported.headers.get("location")!).searchParams.get("error")).toBe(
        "unsupported_response_type",
      );
      const httpsRedirect = "https://bot.example/callback";
      const { client_id: httpsClientId } = yield* register(handler, httpsRedirect, null).pipe(
        Effect.flatMap(json<{ client_id: string }>),
      );
      for (const params of [
        { response_type: "token" },
        { code_challenge_method: "plain" },
        { resource: "https://elsewhere.example/mcp" },
      ]) {
        const shown = yield* authorize({
          ...authorizeParams(httpsClientId, httpsRedirect),
          ...params,
        });
        expect(shown.status, JSON.stringify(params)).toBe(400);
        expect(shown.headers.get("location")).toBeNull();
      }

      for (const resource of [`${ORIGIN}/mcp#section`, `${ORIGIN}/mcp#`]) {
        const wrongTarget = yield* decide(
          handler,
          { ...authorizeParams(clientId), resource },
          decision,
        );
        expect(wrongTarget.status).toBe(200);
        expect(new URL(wrongTarget.redirectTo!).searchParams.get("error"), resource).toBe(
          "invalid_target",
        );
      }
    }),
  ),
);

it.live("signs in with a pairing code and issues a token only /mcp accepts", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      const clientId = yield* registeredClientId(handler);
      const pairing = yield* auth.issuePairingCredential();
      const params = authorizeParams(clientId);

      const wrongCode = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "auto",
        code: "nope",
      });
      expect(wrongCode.status).toBe(400);
      expect(wrongCode.message).toContain("unknown, expired, or already used");

      const approved = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "auto",
        code: pairing.credential,
      });
      expect(approved.status).toBe(200);
      const callback = new URL(approved.redirectTo!);
      expect(callback.origin).toBe("http://localhost:51234");
      expect(callback.searchParams.get("state")).toBe("state-1");
      expect(callback.searchParams.get("iss")).toBe(ORIGIN);
      const code = callback.searchParams.get("code")!;

      const exchange = (codeVerifier: string) =>
        handler(
          at(
            "/oauth/mcp/token",
            form({
              grant_type: "authorization_code",
              code,
              redirect_uri: params.redirect_uri,
              client_id: clientId,
              code_verifier: codeVerifier,
              resource: `${ORIGIN}/mcp`,
            }),
          ),
        );
      const tokenResponse = yield* exchange(verifier);
      expect(tokenResponse.status).toBe(200);
      const token = (yield* json<unknown>(tokenResponse)) as {
        access_token: string;
        scope: string;
      };
      expect(token.scope).toBe("orchestration:read orchestration:operate");

      expect((yield* exchange(verifier)).status).toBe(400);

      for (const scheme of ["Bearer", "bearer", "bEaReR"]) {
        const client = yield* auth.authenticateMcpClient(
          HttpServerRequest.fromWeb(
            at("/mcp", { headers: { authorization: `${scheme} ${token.access_token}` } }),
          ),
        );
        expect(client).toMatchObject({ label: "Workstation", access: "auto", peerLink: true });
      }

      const session = yield* handler(
        at("/api/auth/session", { headers: { authorization: `Bearer ${token.access_token}` } }),
      );
      expect(yield* json<unknown>(session)).toMatchObject({ authenticated: false });
      const ticket = yield* handler(
        at("/api/auth/websocket-ticket", {
          method: "POST",
          headers: { authorization: `Bearer ${token.access_token}` },
        }),
      );
      expect(ticket.status).toBe(401);

      // Revocation ends only an MCP client's own session; any other token is answered alike and kept.
      const owner = yield* auth.exchangeBootstrapCredentialForAccessToken(
        (yield* auth.issuePairingCredential()).credential,
        undefined,
        { deviceType: "desktop" },
      );
      const bearer = (token: string) =>
        HttpServerRequest.fromWeb(at("/api", { headers: { authorization: `Bearer ${token}` } }));
      for (const other of ["not-a-token", owner.access_token]) {
        expect((yield* handler(at("/oauth/mcp/revoke", form({ token: other })))).status).toBe(200);
      }
      yield* auth.authenticateHttpRequest(bearer(owner.access_token));
      const revoked = yield* handler(at("/oauth/mcp/revoke", form({ token: token.access_token })));
      expect(revoked.status).toBe(200);
      yield* auth.authenticateMcpClient(bearer(token.access_token)).pipe(Effect.flip);

      // The thread CLI's own token ends its session the same way.
      const cli = yield* auth.issueMcpClientSession({
        label: "Thread CLI",
        access: "approval-required",
        purpose: "thread-cli",
        client: { deviceType: "bot" },
      });
      yield* auth.authenticateMcpClient(bearer(cli.token));
      yield* handler(at("/oauth/mcp/revoke", form({ token: cli.token })));
      yield* auth.authenticateMcpClient(bearer(cli.token)).pipe(Effect.flip);
    }),
  ),
);

it.live("issues no session for a sign-in approved before all other sessions were revoked", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      const clientId = yield* registeredClientId(handler);
      const params = authorizeParams(clientId);
      const approved = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "auto",
        code: (yield* auth.issuePairingCredential()).credential,
      });
      const code = new URL(approved.redirectTo!).searchParams.get("code")!;
      const owner = yield* auth.issueSession();
      yield* auth.revokeOtherClientSessions(owner.sessionId);

      const response = yield* handler(
        at(
          "/oauth/mcp/token",
          form({
            grant_type: "authorization_code",
            code,
            redirect_uri: params.redirect_uri,
            client_id: clientId,
            code_verifier: verifier,
            resource: `${ORIGIN}/mcp`,
          }),
        ),
      );
      expect(response.status).toBe(400);
      expect(yield* json<unknown>(response)).toEqual({
        error: "invalid_grant",
        error_description: "The authorization code is invalid or expired.",
      });
      expect((yield* auth.listSessions()).map((session) => session.sessionId)).toEqual([
        owner.sessionId,
      ]);
    }),
  ),
);

it.live("rejects a wrong PKCE verifier and spends the code", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      const clientId = yield* registeredClientId(handler);
      const pairing = yield* auth.issuePairingCredential();
      const params = authorizeParams(clientId);
      const approved = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "approval-required",
        code: pairing.credential,
      });
      const code = new URL(approved.redirectTo!).searchParams.get("code")!;
      const exchange = (codeVerifier: string) =>
        handler(
          at(
            "/oauth/mcp/token",
            form({
              grant_type: "authorization_code",
              code,
              redirect_uri: params.redirect_uri,
              client_id: clientId,
              code_verifier: codeVerifier,
            }),
          ),
        );
      const wrong = yield* exchange("b".repeat(64));
      expect(wrong.status).toBe(400);
      expect(yield* json<unknown>(wrong)).toMatchObject({ error: "invalid_grant" });
      expect((yield* exchange(verifier)).status).toBe(400);
    }),
  ),
);

it.live("refuses codes without the scopes it grants", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      const clientId = yield* registeredClientId(handler);
      const params = authorizeParams(clientId);

      const approveWith = (code: string) =>
        decide(handler, params, { _tag: "pairing-code", access: "auto", code });

      const readOnly = yield* auth.issuePairingCredential({ scopes: ["orchestration:read"] });
      const readOnlyResponse = yield* approveWith(readOnly.credential);
      expect(readOnlyResponse.status).toBe(400);
      expect(readOnlyResponse.message).toContain("cannot grant this access");
      const operator = yield* auth.issuePairingCredential({
        scopes: ["orchestration:read", "orchestration:operate"],
      });
      const operatorFull = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "full-access",
        code: operator.credential,
      });
      expect(operatorFull.status).toBe(400);

      const readOnlyApproval = yield* decide(handler, params, {
        _tag: "pairing-code",
        access: "read-only",
        code: readOnly.credential,
      });
      expect(readOnlyApproval.status).toBe(200);
      const readOnlyToken = yield* handler(
        at(
          "/oauth/mcp/token",
          form({
            grant_type: "authorization_code",
            code: new URL(readOnlyApproval.redirectTo!).searchParams.get("code")!,
            redirect_uri: params.redirect_uri,
            client_id: clientId,
            code_verifier: verifier,
            resource: `${ORIGIN}/mcp`,
          }),
        ),
      ).pipe(Effect.flatMap(json<{ access_token: string; scope: string }>));
      expect(readOnlyToken.scope).toBe("orchestration:read");
      const readOnlyClient = yield* auth.authenticateMcpClient(
        HttpServerRequest.fromWeb(
          at("/mcp", { headers: { authorization: `Bearer ${readOnlyToken.access_token}` } }),
        ),
      );
      expect(readOnlyClient.access).toBe("read-only");
    }),
  ),
);

it.live("a code minted with an access level's scopes approves that access", () =>
  withRoutes((handler, auth) =>
    Effect.gen(function* () {
      const params = authorizeParams(yield* registeredClientId(handler));
      for (const access of AuthMcpClientAccess.literals) {
        const code = yield* auth.issuePairingCredential({ scopes: [...mcpClientScopes(access)] });
        const approval = yield* decide(handler, params, {
          _tag: "pairing-code",
          access,
          code: code.credential,
        });
        expect(approval.status, access).toBe(200);
      }
    }),
  ),
);

const outsideClientId = (handler: Handler) =>
  register(handler, REDIRECT, null).pipe(
    Effect.flatMap((response) => json<{ client_id: string }>(response)),
    Effect.map((body) => body.client_id),
  );

/** Starts an outside agent's sign-in and returns the consent page's id. */
const beginSignIn = (handler: Handler, clientId: string) =>
  handler(at(`/oauth/mcp/authorize?${new URLSearchParams(authorizeParams(clientId))}`)).pipe(
    Effect.map((response) => {
      expect(response.status).toBe(303);
      const location = new URL(response.headers.get("location")!, ORIGIN);
      expect(location.pathname).toBe("/oauth/mcp/consent");
      return location.searchParams.get("id")!;
    }),
  );

const consent = (handler: Handler, id: string) =>
  handler(at(`/oauth/mcp/consent?id=${encodeURIComponent(id)}`));

const submitCode = (handler: Handler, id: string, code: string) =>
  handler(at("/oauth/mcp/consent/code", form({ id, code })));

const exchangeFor = (handler: Handler, clientId: string, callback: URL) =>
  handler(
    at(
      "/oauth/mcp/token",
      form({
        grant_type: "authorization_code",
        code: callback.searchParams.get("code")!,
        redirect_uri: authorizeParams(clientId).redirect_uri,
        client_id: clientId,
        code_verifier: verifier,
        resource: `${ORIGIN}/mcp`,
      }),
    ),
  ).pipe(Effect.flatMap((response) => json<{ access_token: string; scope: string }>(response)));

it.live("an outside agent signs in once the app approves the code its page shows", () =>
  withRoutes((handler, auth, oauth) =>
    Effect.gen(function* () {
      const clientId = yield* outsideClientId(handler);
      // Only a peer link decides with a pairing code; an outside agent goes through the page.
      const decided = yield* decide(handler, authorizeParams(clientId), {
        _tag: "pairing-code",
        access: "auto",
        code: (yield* auth.issuePairingCredential()).credential,
      });
      expect(decided.status).toBe(400);

      const id = yield* beginSignIn(handler, clientId);
      const waiting = yield* consent(handler, id);
      expect(waiting.status).toBe(200);
      expect(waiting.headers.get("content-security-policy")).toContain("default-src 'none'");
      const page = yield* Effect.promise(() => waiting.text());
      const [pending] = Option.getOrThrow(yield* Stream.runHead(oauth.signIns));
      expect(pending).toMatchObject({ id, clientName: "Workstation", loopbackRedirect: true });
      expect(page).toContain(pending!.matchCode);

      // An approving device grants no more than its own session holds.
      const overreach = yield* oauth
        .answerSignIn({
          id,
          answer: { _tag: "approve", access: "approval-required" },
          grantorScopes: mcpClientScopes("read-only"),
        })
        .pipe(Effect.flip);
      expect(overreach.reason).toBe("scope_required");
      yield* oauth.answerSignIn({
        id,
        answer: { _tag: "approve", access: "approval-required" },
        grantorScopes: mcpClientScopes("approval-required"),
      });
      expect(Option.getOrThrow(yield* Stream.runHead(oauth.signIns))).toEqual([]);
      const finished = yield* consent(handler, id);
      expect(finished.status).toBe(302);
      const callback = new URL(finished.headers.get("location")!);
      expect(callback.searchParams.get("state")).toBe("state-1");
      expect(callback.searchParams.get("iss")).toBe(ORIGIN);
      expect((yield* consent(handler, id)).status).toBe(404);
      const again = yield* oauth
        .answerSignIn({ id, answer: { _tag: "deny" }, grantorScopes: [] })
        .pipe(Effect.flip);
      expect(again.reason).toBe("not_found");

      const token = yield* exchangeFor(handler, clientId, callback);
      expect(token.scope).toBe("orchestration:read orchestration:operate");
      const bearer = HttpServerRequest.fromWeb(
        at("/mcp", { headers: { authorization: `Bearer ${token.access_token}` } }),
      );
      expect(yield* auth.authenticateMcpClient(bearer)).toMatchObject({
        label: "Workstation",
        access: "approval-required",
        peerLink: false,
      });
      // Its token works only on /mcp.
      const ticket = yield* handler(
        at("/api/auth/websocket-ticket", {
          method: "POST",
          headers: { authorization: `Bearer ${token.access_token}` },
        }),
      );
      expect(ticket.status).toBe(401);
    }),
  ),
);

it.live("denying, or five wrong codes, ends an outside agent's sign-in", () =>
  withRoutes((handler, _auth, oauth) =>
    Effect.gen(function* () {
      const clientId = yield* outsideClientId(handler);
      const denied = yield* beginSignIn(handler, clientId);
      yield* oauth.answerSignIn({ id: denied, answer: { _tag: "deny" }, grantorScopes: [] });
      const deniedPage = yield* consent(handler, denied);
      expect(new URL(deniedPage.headers.get("location")!).searchParams.get("error")).toBe(
        "access_denied",
      );

      const guessed = yield* beginSignIn(handler, clientId);
      for (const left of [4, 3, 2, 1]) {
        const wrong = yield* submitCode(handler, guessed, "SUPERVISED-NOTACODE");
        expect(wrong.status).toBe(400);
        expect(yield* Effect.promise(() => wrong.text())).toContain(
          left === 1 ? "One try is left" : `${left} tries are left`,
        );
      }
      expect((yield* submitCode(handler, guessed, "garbage")).status).toBe(303);
      const ended = yield* consent(handler, guessed);
      expect(new URL(ended.headers.get("location")!).searchParams.get("error")).toBe(
        "access_denied",
      );
    }),
  ),
);

it.effect("codes posted together are checked one at a time, within the try limit", () =>
  Effect.gen(function* () {
    const gate = yield* makeApprovalCodeGate;
    yield* withRoutes(
      (handler) =>
        Effect.gen(function* () {
          const id = yield* beginSignIn(handler, yield* outsideClientId(handler));
          const first = yield* submitCode(handler, id, "SUPERVISED-NOTACODE").pipe(
            Effect.forkChild,
          );
          yield* Deferred.await(gate.entered);
          const others = yield* Effect.all(
            Array.from({ length: 9 }, () => submitCode(handler, id, "SUPERVISED-NOTACODE")),
            { concurrency: "unbounded" },
          );
          // Each lands back on the waiting page without a check or a try of its own.
          expect(others.map((response) => response.status)).toEqual(
            Array.from({ length: 9 }, () => 200),
          );
          yield* Deferred.succeed(gate.release, undefined);
          const checked = yield* Fiber.join(first);
          expect(checked.status).toBe(400);
          expect(yield* Effect.promise(() => checked.text())).toContain("4 tries are left");
          expect(yield* Ref.get(gate.checks)).toBe(1);
        }),
      { gate },
    );
  }),
);

it.live("an answer from the app wins over a code still being checked", () =>
  Effect.gen(function* () {
    const gate = yield* makeApprovalCodeGate;
    yield* withRoutes(
      (handler, auth, oauth) =>
        Effect.gen(function* () {
          const clientId = yield* outsideClientId(handler);
          const id = yield* beginSignIn(handler, clientId);
          const agentCode = auth
            .issuePairingCredential({ purpose: "agent", access: "read-only" })
            .pipe(Effect.map((issued) => mcpApprovalCode("read-only", issued.credential)));
          const checking = yield* agentCode;
          const spare = yield* agentCode;
          const first = yield* submitCode(handler, id, checking).pipe(Effect.forkChild);
          yield* Deferred.await(gate.entered);
          // A second code waits on the first rather than being spent beside it.
          expect((yield* submitCode(handler, id, spare)).status).toBe(200);
          yield* oauth.answerSignIn({ id, answer: { _tag: "deny" }, grantorScopes: [] });
          yield* Deferred.succeed(gate.release, undefined);
          expect((yield* Fiber.join(first)).status).toBe(303);
          const ended = yield* consent(handler, id);
          expect(new URL(ended.headers.get("location")!).searchParams.get("error")).toBe(
            "access_denied",
          );

          const next = yield* beginSignIn(handler, clientId);
          expect((yield* submitCode(handler, next, spare)).status).toBe(303);
          const approved = new URL((yield* consent(handler, next)).headers.get("location")!);
          expect(approved.searchParams.get("code")).not.toBeNull();
        }),
      { gate },
    );
  }),
);

it.live("a refused code leaves a finished sign-in to the page's own load", () =>
  withRoutes((handler, _auth, oauth) =>
    Effect.gen(function* () {
      const id = yield* beginSignIn(handler, yield* outsideClientId(handler));
      for (let post = 0; post < 30; post += 1) {
        yield* submitCode(handler, "no-such-sign-in", "SUPERVISED-NOTACODE");
      }
      yield* oauth.answerSignIn({ id, answer: { _tag: "deny" }, grantorScopes: [] });

      // Over the rate limit. CSP's form-action would stop a redirect off-origin from this POST.
      const refused = yield* submitCode(handler, id, "SUPERVISED-NOTACODE");
      expect(refused.status).toBe(303);
      expect(refused.headers.get("location")).toContain("/oauth/mcp/consent?id=");
      const ended = yield* consent(handler, id);
      expect(new URL(ended.headers.get("location")!).searchParams.get("error")).toBe(
        "access_denied",
      );
    }),
  ),
);

it.live("a code check finishes though its request is interrupted", () =>
  Effect.gen(function* () {
    const gate = yield* makeApprovalCodeGate;
    yield* withRoutes(
      (handler, auth, oauth) =>
        Effect.gen(function* () {
          const id = yield* beginSignIn(handler, yield* outsideClientId(handler));
          const issued = yield* auth.issuePairingCredential({
            purpose: "agent",
            access: "read-only",
          });
          const check = yield* oauth
            .approveSignInWithCode({ id, code: mcpApprovalCode("read-only", issued.credential) })
            .pipe(Effect.forkChild);
          yield* Deferred.await(gate.entered);
          const interrupting = yield* Fiber.interrupt(check).pipe(
            Effect.forkChild({ startImmediately: true }),
          );
          yield* Deferred.succeed(gate.release, undefined);
          yield* Fiber.join(interrupting);

          const approved = new URL((yield* consent(handler, id)).headers.get("location")!);
          expect(approved.searchParams.get("code")).not.toBeNull();
        }),
      { gate },
    );
  }),
);

it.effect("an expired sign-in leaves the waiting list when it expires", () =>
  withRoutes((handler, _auth, oauth) =>
    Effect.gen(function* () {
      const authorization = yield* oauth.validateAuthorization({
        urls: { issuer: ORIGIN, resource: `${ORIGIN}/mcp` },
        request: yield* decodeAuthorizationRequest(
          authorizeParams(yield* outsideClientId(handler)),
        ),
      });
      const signIn = yield* oauth.beginSignIn(authorization);
      const subscribed = yield* Deferred.make<void>();
      const seen = yield* oauth.signIns.pipe(
        Stream.tap(() => Deferred.succeed(subscribed, undefined)),
        Stream.take(2),
        Stream.runCollect,
        Effect.forkChild,
      );
      yield* Deferred.await(subscribed);
      yield* TestClock.adjust("10 minutes");
      expect(Option.getOrThrow(yield* Stream.runHead(oauth.signIns))).toEqual([]);
      expect(
        Array.from(yield* Fiber.join(seen), (requests) => requests.map((request) => request.id)),
      ).toEqual([[signIn.id], []]);
      expect((yield* consent(handler, signIn.id)).status).toBe(404);
    }),
  ),
);

it.live("an agent code approves a sign-in at its level, and a peer-link code does not", () =>
  withRoutes((handler, auth, oauth) =>
    Effect.gen(function* () {
      const clientId = yield* outsideClientId(handler);
      const id = yield* beginSignIn(handler, clientId);
      const peerCode = yield* auth.issuePairingCredential({
        purpose: "peer-link",
        access: "read-only",
      });
      expect(
        (yield* submitCode(handler, id, mcpApprovalCode("read-only", peerCode.credential))).status,
      ).toBe(400);

      const agentCode = yield* auth.issuePairingCredential({
        purpose: "agent",
        access: "read-only",
      });
      expect(agentCode).toMatchObject({ purpose: "agent", access: "read-only" });
      const typed = mcpApprovalCode("read-only", agentCode.credential).toLowerCase();
      expect((yield* submitCode(handler, id, ` ${typed} `)).status).toBe(303);
      const finished = yield* consent(handler, id);
      expect(finished.status).toBe(302);
      const token = yield* exchangeFor(
        handler,
        clientId,
        new URL(finished.headers.get("location")!),
      );
      expect(token.scope).toBe("orchestration:read");

      const tokenRequest = {
        label: "Cron job",
        access: "full-access",
        client: { deviceType: "bot" },
      } as const;
      const overreach = yield* oauth
        .issueAgentToken({ ...tokenRequest, grantorScopes: mcpClientScopes("auto") })
        .pipe(Effect.flip);
      expect(overreach.reason).toBe("scope_required");
      const issued = yield* oauth.issueAgentToken({
        ...tokenRequest,
        grantorScopes: mcpClientScopes("full-access"),
      });
      expect(
        yield* auth.authenticateMcpClient(
          HttpServerRequest.fromWeb(
            at("/mcp", { headers: { authorization: `Bearer ${issued.token}` } }),
          ),
        ),
      ).toMatchObject({ label: "Cron job", access: "full-access", peerLink: false });
    }),
  ),
);

it.live("a policy that keeps remote clients out refuses outside agents", () =>
  withRoutes(
    (handler, _auth, oauth) =>
      Effect.gen(function* () {
        const clientId = yield* outsideClientId(handler);
        const page = yield* handler(
          at(`/oauth/mcp/authorize?${new URLSearchParams(authorizeParams(clientId))}`),
        );
        expect(page.status).toBe(403);
        expect(yield* Effect.promise(() => page.text())).toContain(
          "Example Corp does not allow outside agents",
        );
        const token = yield* oauth
          .issueAgentToken({
            label: "Cron job",
            access: "read-only",
            client: { deviceType: "bot" },
            grantorScopes: mcpClientScopes("full-access"),
          })
          .pipe(Effect.flip);
        expect(token.reason).toBe("policy_denied");
      }),
    {
      policy: {
        schemaVersion: 1,
        organization: "Example Corp",
        limits: { clients: ["desktop", "local"] },
      },
    },
  ),
);

it.effect("serves discovery metadata for the host it was called on", () =>
  withRoutes((handler) =>
    Effect.gen(function* () {
      const resource = yield* json<{ resource: string; authorization_servers: string[] }>(
        yield* handler(at("/.well-known/oauth-protected-resource/mcp")),
      );
      expect(resource).toMatchObject({
        resource: `${ORIGIN}/mcp`,
        authorization_servers: [ORIGIN],
      });
      const server = yield* json<{ authorization_endpoint: string }>(
        yield* handler(at("/.well-known/oauth-authorization-server")),
      );
      expect(server.authorization_endpoint).toBe(`${ORIGIN}/oauth/mcp/authorize`);
    }),
  ),
);

it("matches loopback redirects on everything but the port, and https redirects exactly", () => {
  const matches = McpOAuth.redirectMatches;
  expect(matches("http://localhost/callback", "http://localhost:9/callback")).toBe(true);
  expect(matches("http://127.0.0.1:1/callback", "http://127.0.0.1:2/callback")).toBe(true);
  expect(matches("http://localhost/callback", "http://127.0.0.1/callback")).toBe(false);
  expect(matches("http://localhost/callback", "https://localhost/callback")).toBe(false);
  expect(matches("https://bot.example/cb", "https://bot.example/cb")).toBe(true);
  expect(matches("https://bot.example/cb", "https://bot.example:8443/cb")).toBe(false);
  expect(matches("https://bot.example/cb", "https://bot.example/cb?x=1")).toBe(false);
  expect(matches("https://bot.example/cb", "https://evil.example/cb")).toBe(false);
});
