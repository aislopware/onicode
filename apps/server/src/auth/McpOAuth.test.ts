import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import {
  AuthMcpClientAccess,
  EnvironmentHttpApi,
  MCP_PEER_LINK_SOFTWARE_ID,
  mcpClientScopes,
} from "@onicode/contracts";
import { expect, it } from "@effect/vitest";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Etag from "effect/http/Etag";
import * as HttpPlatform from "effect/http/HttpPlatform";
import * as HttpRouter from "effect/http/HttpRouter";
import * as HttpServerRequest from "effect/http/HttpServerRequest";
import * as HttpApi from "effect/http-api/HttpApi";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";
import * as Schema from "effect/Schema";

import * as ServerConfig from "../config.ts";
import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as Sqlite from "../persistence/Sqlite.ts";
import * as EnvironmentAuth from "./EnvironmentAuth.ts";
import * as McpOAuth from "./McpOAuth.ts";
import * as McpOAuthHttp from "./mcpOAuthHttp.ts";
import * as ServerSecretStore from "./ServerSecretStore.ts";
import * as AuthHttp from "./http.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";

class AuthTestApi extends HttpApi.make("environment")
  .add(EnvironmentHttpApi.groups.auth)
  .add(EnvironmentHttpApi.groups.mcpOAuth) {}

const layerConfig = ServerConfig.layerTest(process.cwd(), { prefix: "oni-mcp-oauth-test-" });
const layerEnvironmentAuth = EnvironmentAuth.layer.pipe(
  Layer.provide(ManagedPolicy.layerTest()),
  Layer.provide(Sqlite.layerMemory),
  Layer.provideMerge(ServerSecretStore.layer),
  Layer.provideMerge(ServerEnvironment.layerIdentity),
  Layer.provide(layerConfig),
);
const makeLayerRoutes = (capture: (auth: EnvironmentAuth.EnvironmentAuth["Service"]) => void) =>
  Layer.mergeAll(
    Layer.effectDiscard(
      EnvironmentAuth.EnvironmentAuth.pipe(Effect.tap((auth) => Effect.sync(() => capture(auth)))),
    ),
    HttpApiBuilder.layer(AuthTestApi).pipe(
      Layer.provide(AuthHttp.layer),
      Layer.provide(
        McpOAuthHttp.layer.pipe(
          Layer.provide(McpOAuth.layer),
          Layer.provide(ServerSettings.layerTest({ linkedEnvironmentsEnabled: true })),
        ),
      ),
      Layer.provide(AuthHttp.layerAuthenticatedAuth),
    ),
  ).pipe(
    Layer.provideMerge(layerEnvironmentAuth),
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
  use: (handler: Handler, auth: EnvironmentAuth.EnvironmentAuth["Service"]) => Effect.Effect<A, E>,
) =>
  Effect.gen(function* () {
    const random = yield* SecureRandom.SecureRandom;
    const context = Context.make(SecureRandom.SecureRandom, random);
    let routerAuth: EnvironmentAuth.EnvironmentAuth["Service"] | undefined;
    const layerRoutes = makeLayerRoutes((auth) => {
      routerAuth = auth;
    });
    return yield* Effect.acquireUseRelease(
      Effect.sync(() => HttpRouter.toWebHandler(layerRoutes, { disableLogger: true })),
      (web) =>
        Effect.gen(function* () {
          const handler: Handler = (request) => Effect.promise(() => web.handler(request, context));
          yield* handler(at("/api/auth/session"));
          return yield* use(handler, routerAuth!);
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

it.live("registers only peer links and never redirects for an unverified client", () =>
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
        const outsideAgent = yield* register(handler, REDIRECT, softwareId);
        expect(outsideAgent.status).toBe(400);
        expect(yield* json<unknown>(outsideAgent)).toMatchObject({
          error: "invalid_client_metadata",
        });
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
