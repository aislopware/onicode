import {
  type AgentSignInAnswer,
  AgentSignInError,
  type AgentSignInRequest,
  type AuthMcpApprovalDecision,
  type AuthMcpAuthorizationRequest,
  type AuthMcpAuthorizationServerMetadata,
  type AuthMcpClientRegistration,
  type AuthMcpProtectedResourceMetadata,
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  AuthMcpRegistrationError,
  AuthMcpTokenError,
  type AuthMcpTokenRequest,
  type AuthMcpTokenResult,
  type AuthMcpClientAccess,
  MCP_PEER_LINK_SOFTWARE_ID,
  mcpClientScopes,
  parseMcpApprovalCode,
} from "@onicode/contracts";
import { encodeOAuthScope } from "@onicode/shared/oauthScope";
import * as Clock from "effect/Clock";
import * as DateTime from "effect/DateTime";
import * as Context from "effect/Context";
import * as Digest from "effect/crypto/Digest";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Redacted from "effect/Redacted";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { HttpServerRequest } from "effect/http";

import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as McpHttpServer from "../mcp/McpHttpServer.ts";
import type * as McpInvocationContext from "../mcp/McpInvocationContext.ts";
import type { AuthClientMetadata, AuthEnvironmentScope } from "@onicode/contracts";
import * as PayloadSigner from "./PayloadSigner.ts";
import * as EnvironmentAuth from "./EnvironmentAuth.ts";
import * as ServerSecretStore from "./ServerSecretStore.ts";
import {
  base64UrlDecodeUtf8,
  base64UrlEncode,
  deriveAuthClientMetadata,
  timingSafeEqualBase64Url,
} from "./utils.ts";
import { requestUrl } from "../requestUrl.ts";

// Outside agents sign in on the consent page this server renders, approved from a signed-in app or
// with an agent code. Another Onicode environment links by deciding with a pairing code instead.
// Signed client metadata keeps registration stateless. Request-derived origins support loopback,
// Tailscale and Funnel.

const SIGNING_SECRET_NAME = "mcp-oauth-signing-key";
const AUTHORIZATION_CODE_TTL_MS = 60_000;
const SIGN_IN_TTL_MS = 10 * 60_000;
const MAX_PENDING_SIGN_INS = 20;
const MAX_CODE_ATTEMPTS = 5;
// Every route here is unauthenticated, and Funnel can put them on the internet.
const REGISTRATIONS_PER_MINUTE = 30;
const SIGN_INS_PER_MINUTE = 30;
const TOKEN_EXCHANGES_PER_MINUTE = 60;
const CODE_SUBMISSIONS_PER_MINUTE = 30;
const DECISIONS_PER_MINUTE = 30;
const MAX_CLIENT_NAME_LENGTH = 100;
const MAX_REDIRECT_URIS = 10;
const MAX_REDIRECT_URI_LENGTH = 512;
// The client_id rides in the authorize URL. Past five redirect URIs, cap it to leave room under
// Node's 16 KB header cap for an encoded redirect_uri and state.
const MAX_UNBOUNDED_CLIENT_ID_REDIRECT_URIS = 5;
const MAX_CLIENT_ID_LENGTH = 10_240;
const DEFAULT_CLIENT_NAME = "MCP client";
const MATCH_CODE_ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const MCP_OAUTH_SCOPES = [AuthOrchestrationReadScope, AuthOrchestrationOperateScope];
const LOOPBACK_HOSTNAMES = new Set(["localhost", "127.0.0.1", "[::1]"]);
const CODE_CHALLENGE_PATTERN = /^[A-Za-z0-9_-]{43}$/;
const CODE_VERIFIER_PATTERN = /^[A-Za-z0-9\-._~]{43,128}$/;

export interface McpOAuthUrls {
  readonly issuer: string;
  readonly resource: string;
}

export const requestUrls = (request: HttpServerRequest.HttpServerRequest): McpOAuthUrls => {
  const origin = Option.match(requestUrl(request), {
    onNone: () => "http://localhost",
    onSome: (url) => url.origin,
  });
  return { issuer: origin, resource: `${origin}/mcp` };
};

export const protectedResourceMetadata = (
  urls: McpOAuthUrls,
): AuthMcpProtectedResourceMetadata => ({
  resource: urls.resource,
  authorization_servers: [urls.issuer],
  scopes_supported: MCP_OAUTH_SCOPES,
  bearer_methods_supported: ["header"],
  resource_name: "Onicode",
});

export const authorizationServerMetadata = (
  urls: McpOAuthUrls,
): AuthMcpAuthorizationServerMetadata => ({
  issuer: urls.issuer,
  authorization_endpoint: `${urls.issuer}/oauth/mcp/authorize`,
  token_endpoint: `${urls.issuer}/oauth/mcp/token`,
  registration_endpoint: `${urls.issuer}/oauth/mcp/register`,
  revocation_endpoint: `${urls.issuer}/oauth/mcp/revoke`,
  response_types_supported: ["code"],
  grant_types_supported: ["authorization_code"],
  code_challenge_methods_supported: ["S256"],
  token_endpoint_auth_methods_supported: ["none"],
  scopes_supported: MCP_OAUTH_SCOPES,
  authorization_response_iss_parameter_supported: true,
});

const parseRedirect = (value: string): URL | undefined => {
  // URL.hash misses empty fragments, which RFC 6749 §3.1.2 also forbids.
  if (value.length > MAX_REDIRECT_URI_LENGTH || value.includes("#")) return undefined;
  try {
    const url = new URL(value);
    const allowed =
      url.protocol === "https:" ||
      (url.protocol === "http:" && LOOPBACK_HOSTNAMES.has(url.hostname));
    return allowed && url.username === "" && url.password === "" ? url : undefined;
  } catch {
    return undefined;
  }
};

const isLoopbackRedirect = (value: string): boolean => {
  const url = parseRedirect(value);
  return url !== undefined && LOOPBACK_HOSTNAMES.has(url.hostname);
};

/** Loopback redirects ignore port (RFC 8252 §7.3); HTTPS redirects match exactly. */
export const redirectMatches = (registered: string, presented: string): boolean => {
  const left = parseRedirect(registered);
  const right = parseRedirect(presented);
  if (left === undefined || right === undefined) return false;
  if (left.protocol === "https:" || right.protocol === "https:") return left.href === right.href;
  return (
    left.hostname === right.hostname &&
    left.pathname === right.pathname &&
    left.search === right.search
  );
};

const sameResource = (urls: McpOAuthUrls, presented: string | undefined): boolean => {
  if (presented === undefined) return true;
  // URL.hash misses empty fragments, which RFC 8707 §2 also forbids.
  if (presented.includes("#")) return false;
  try {
    const url = new URL(presented);
    return `${url.origin}${url.pathname.replace(/\/+$/u, "")}` === urls.resource;
  } catch {
    return false;
  }
};

const ClientIdPayload = Schema.Struct({
  v: Schema.Literal(1),
  n: Schema.String,
  r: Schema.Array(Schema.String),
  /** Registered as another Onicode environment's peer link. */
  p: Schema.optionalKey(Schema.Literal(1)),
});
const decodeClientIdPayload = Schema.decodeUnknownOption(Schema.fromJsonString(ClientIdPayload));

export interface McpOAuthClient {
  readonly clientId: string;
  readonly name: string;
  readonly redirectUris: ReadonlyArray<string>;
  readonly peerLink: boolean;
}

export class McpOAuthPageError extends Schema.TaggedError<McpOAuthPageError>()(
  "McpOAuthPageError",
  { description: Schema.String, busy: Schema.optionalKey(Schema.Boolean) },
) {
  override get message(): string {
    return this.description;
  }
}

export class McpOAuthRedirectError extends Schema.TaggedError<McpOAuthRedirectError>()(
  "McpOAuthRedirectError",
  {
    error: Schema.Literals(["invalid_request", "unsupported_response_type", "invalid_target"]),
    description: Schema.String,
    redirectUri: Schema.String,
    state: Schema.optional(Schema.RedactedFromValue(Schema.String)),
  },
) {}

export interface AuthorizationRequest {
  readonly client: McpOAuthClient;
  readonly redirectUri: string;
  readonly codeChallenge: string;
  readonly state: Redacted.Redacted<string> | undefined;
  readonly resource: string;
  readonly issuer: string;
}

interface PendingCode {
  readonly clientId: string;
  readonly clientName: string;
  readonly peerLink: boolean;
  readonly redirectUri: string;
  readonly codeChallenge: string;
  readonly resource: string;
  readonly access: AuthMcpClientAccess;
  readonly approvedAtRevocationCount: number;
  readonly expiresAtMs: number;
}

/** A sign-in waiting on the consent page, as the page shows it. */
export interface SignInView {
  readonly id: string;
  readonly clientName: string;
  readonly redirectHost: string;
  readonly loopbackRedirect: boolean;
  readonly matchCode: string;
  readonly codeAttemptsLeft: number;
}

export type SignInPage =
  | { readonly _tag: "waiting"; readonly signIn: SignInView }
  | { readonly _tag: "finished"; readonly redirectTo: string }
  | { readonly _tag: "missing" };

interface SignIn extends SignInView {
  readonly authorization: AuthorizationRequest;
  readonly expiresAtMs: number;
  /** Set once answered; the page's next load redirects there and the sign-in ends. */
  readonly redirectTo?: string;
  /** An agent code is being checked; its try is already spent. */
  readonly checkingCode?: boolean;
}

export class McpOAuth extends Context.Service<
  McpOAuth,
  {
    readonly register: (
      input: AuthMcpClientRegistration,
    ) => Effect.Effect<McpOAuthClient, AuthMcpRegistrationError>;
    /** Page errors must never redirect. */
    readonly validateAuthorization: (input: {
      readonly urls: McpOAuthUrls;
      readonly request: AuthMcpAuthorizationRequest;
    }) => Effect.Effect<AuthorizationRequest, McpOAuthPageError | McpOAuthRedirectError>;
    /** A peer link's sign-in, decided with its pairing code. */
    readonly approve: (input: {
      readonly authorization: AuthorizationRequest;
      readonly decision: AuthMcpApprovalDecision;
    }) => Effect.Effect<string, EnvironmentAuth.ServerAuthMcpApprovalCodeError | McpOAuthPageError>;
    /** Holds an outside agent's validated sign-in until the user answers it. */
    readonly beginSignIn: (
      authorization: AuthorizationRequest,
    ) => Effect.Effect<SignInView, McpOAuthPageError>;
    /** The page's view of a sign-in; a finished one ends when this returns its redirect. */
    readonly signInPage: (id: string) => Effect.Effect<SignInPage>;
    /** The page's view without ending a finished sign-in; the page's own next load does that. */
    readonly peekSignIn: (id: string) => Effect.Effect<SignInPage>;
    readonly approveSignInWithCode: (input: {
      readonly id: string;
      readonly code: string;
    }) => Effect.Effect<SignInPage, McpOAuthPageError>;
    /** `grantorScopes` are the answering session's: it cannot grant more than it holds. */
    readonly answerSignIn: (input: {
      readonly id: string;
      readonly answer: AgentSignInAnswer;
      readonly grantorScopes: ReadonlyArray<AuthEnvironmentScope>;
    }) => Effect.Effect<void, AgentSignInError>;
    /** Sign-ins waiting for an answer, for clients that may approve them. */
    readonly signIns: Stream.Stream<ReadonlyArray<AgentSignInRequest>>;
    /** A bearer token for an agent that takes a static header instead of signing in. */
    readonly issueAgentToken: (input: {
      readonly label: string;
      readonly access: AuthMcpClientAccess;
      readonly client: AuthClientMetadata;
      readonly grantorScopes: ReadonlyArray<AuthEnvironmentScope>;
    }) => Effect.Effect<
      { readonly token: string; readonly expiresAt: DateTime.Utc },
      AgentSignInError
    >;
    readonly exchangeCode: (input: {
      readonly request: HttpServerRequest.HttpServerRequest;
      readonly urls: McpOAuthUrls;
      readonly token: AuthMcpTokenRequest;
    }) => Effect.Effect<AuthMcpTokenResult, AuthMcpTokenError>;
  }
>()("onicode/auth/McpOAuth") {}

const redirectWith = (
  redirectUri: string,
  params: Record<string, string | Redacted.Redacted<string> | undefined>,
) => {
  const url = new URL(redirectUri);
  for (const [name, value] of Object.entries(params)) {
    if (value !== undefined) {
      url.searchParams.set(name, Redacted.isRedacted(value) ? Redacted.value(value) : value);
    }
  }
  return url.toString();
};

export const redirectForError = (error: McpOAuthRedirectError, issuer: string) =>
  redirectWith(error.redirectUri, {
    error: error.error,
    error_description: error.description,
    state: error.state,
    iss: issuer,
  });

const make = Effect.gen(function* () {
  const environmentAuth = yield* EnvironmentAuth.EnvironmentAuth;
  const secretStore = yield* ServerSecretStore.ServerSecretStore;
  const random = yield* SecureRandom.SecureRandom;
  const digester = yield* Digest.Digest;
  const serverSettings = yield* ServerSettings.ServerSettingsService;
  const checkGrant = Effect.fnUntraced(function* (
    access: AuthMcpClientAccess,
    grantorScopes: ReadonlyArray<AuthEnvironmentScope>,
  ) {
    if ((yield* environmentAuth.outsideAgentsForbiddenBy) !== null) {
      return yield* new AgentSignInError({ reason: "policy_denied" });
    }
    if (!mcpClientScopes(access).every((scope) => grantorScopes.includes(scope))) {
      return yield* new AgentSignInError({ reason: "scope_required" });
    }
  });
  const linksEnabled = serverSettings.getSettings.pipe(
    Effect.map((settings) => settings.linkedEnvironmentsEnabled),
    Effect.orElseSucceed(() => false),
  );
  const signer = yield* PayloadSigner.fromSecret(
    yield* secretStore.getOrCreateRandom(SIGNING_SECRET_NAME, 32).pipe(Effect.orDie),
  );
  const codes = yield* Ref.make<ReadonlyMap<string, PendingCode>>(new Map());

  const sign = (domain: string, payload: string) => signer.sign(`${domain}.${payload}`);

  const signClientId = (name: string, redirectUris: ReadonlyArray<string>, peerLink: boolean) => {
    const body = base64UrlEncode(
      JSON.stringify({ v: 1, n: name, r: redirectUris, ...(peerLink ? { p: 1 } : {}) }),
    );
    return sign("mcp-client-id", body).pipe(Effect.map((signature) => `${body}.${signature}`));
  };

  /** True while `limit` uses are left in the current minute. */
  const perMinute = (limit: number) =>
    Ref.make({ windowStartMs: 0, used: 0 }).pipe(
      Effect.map((window) =>
        Clock.currentTimeMillis.pipe(
          Effect.flatMap((now) =>
            Ref.modify(window, (current) => {
              const fresh =
                now - current.windowStartMs >= 60_000 ? { windowStartMs: now, used: 0 } : current;
              return fresh.used >= limit
                ? [false, fresh]
                : [true, { ...fresh, used: fresh.used + 1 }];
            }),
          ),
        ),
      ),
    );
  const registrationAllowed = yield* perMinute(REGISTRATIONS_PER_MINUTE);
  const signInAllowed = yield* perMinute(SIGN_INS_PER_MINUTE);
  const tokenExchangeAllowed = yield* perMinute(TOKEN_EXCHANGES_PER_MINUTE);
  const codeSubmissionAllowed = yield* perMinute(CODE_SUBMISSIONS_PER_MINUTE);
  const decisionAllowed = yield* perMinute(DECISIONS_PER_MINUTE);
  const serviceScope = yield* Effect.scope;

  // Untraced: only called inside the traced McpOAuth methods below.
  const parseClientId = Effect.fnUntraced(function* (
    clientId: string,
  ): Effect.fn.Return<McpOAuthClient | undefined> {
    const [body, signature, extra] = clientId.split(".");
    if (!body || !signature || extra !== undefined) return undefined;
    const verified = yield* signer.verify(`mcp-client-id.${body}`, signature);
    if (!verified) return undefined;
    const json = Option.liftThrowable(base64UrlDecodeUtf8)(body);
    if (Option.isNone(json)) return undefined;
    return Option.getOrUndefined(
      decodeClientIdPayload(json.value).pipe(
        Option.map((payload) => ({
          clientId,
          name: payload.n,
          redirectUris: payload.r,
          peerLink: payload.p === 1,
        })),
      ),
    );
  });

  const pkceVerifies = (verifier: string, challenge: string) =>
    CODE_VERIFIER_PATTERN.test(verifier)
      ? digester.digest(Digest.sha256, new TextEncoder().encode(verifier)).pipe(
          Effect.orDie,
          Effect.map((digest) => timingSafeEqualBase64Url(base64UrlEncode(digest), challenge)),
        )
      : Effect.succeed(false);

  const register: McpOAuth["Service"]["register"] = Effect.fn("McpOAuth.register")(
    function* (metadata) {
      if (!(yield* registrationAllowed)) {
        return yield* new AuthMcpRegistrationError({
          error: "invalid_client_metadata",
          error_description: "Too many apps registered just now. Try again in a minute.",
        });
      }
      const rawName = metadata.client_name?.trim() ?? "";
      const name = (rawName.length > 0 ? rawName : DEFAULT_CLIENT_NAME).slice(
        0,
        MAX_CLIENT_NAME_LENGTH,
      );
      const redirectUris = metadata.redirect_uris ?? [];
      if (redirectUris.length === 0 || redirectUris.length > MAX_REDIRECT_URIS) {
        return yield* new AuthMcpRegistrationError({
          error: "invalid_redirect_uri",
          error_description: `Register between 1 and ${MAX_REDIRECT_URIS} redirect URIs.`,
        });
      }
      if (!redirectUris.every((uri) => parseRedirect(uri) !== undefined)) {
        return yield* new AuthMcpRegistrationError({
          error: "invalid_redirect_uri",
          error_description:
            "Redirect URIs must be https, or http on localhost, 127.0.0.1 or [::1].",
        });
      }
      // RFC 7591 allows ignoring unsupported registration options; all clients use public PKCE.
      const peerLink = metadata.software_id === MCP_PEER_LINK_SOFTWARE_ID;
      if (peerLink && !(yield* linksEnabled)) {
        return yield* new AuthMcpRegistrationError({
          error: "invalid_client_metadata",
          error_description:
            'Linked environments is off on that machine. Turn it on in its Settings → Labs, or without a desktop, set "linkedEnvironmentsEnabled": true in userdata/settings.json in its Onicode home (~/.onicode by default).',
        });
      }
      const clientId = yield* signClientId(name, redirectUris, peerLink);
      if (
        redirectUris.length > MAX_UNBOUNDED_CLIENT_ID_REDIRECT_URIS &&
        clientId.length > MAX_CLIENT_ID_LENGTH
      ) {
        return yield* new AuthMcpRegistrationError({
          error: "invalid_redirect_uri",
          error_description: "Redirect URIs are too long to fit in an authorization request.",
        });
      }
      return { clientId, name, redirectUris, peerLink };
    },
  );

  const validateAuthorization: McpOAuth["Service"]["validateAuthorization"] = Effect.fn(
    "McpOAuth.validateAuthorization",
  )(function* ({ urls, request }) {
    const client = yield* parseClientId(request.client_id ?? "");
    if (client === undefined) {
      return yield* new McpOAuthPageError({
        description: "This sign-in link names an unknown app. Start the sign-in again from it.",
      });
    }
    const redirectUri = request.redirect_uri;
    if (
      redirectUri === undefined ||
      !client.redirectUris.some((registered) => redirectMatches(registered, redirectUri))
    ) {
      return yield* new McpOAuthPageError({
        description: "This sign-in link sends you to an address the app did not register.",
      });
    }
    const state = request.state;
    // Anyone can register an https redirect, so only a loopback one is trusted with an
    // automatic error redirect (RFC 9700 §4.11.2).
    const fail = (error: McpOAuthRedirectError["error"], description: string) =>
      isLoopbackRedirect(redirectUri)
        ? new McpOAuthRedirectError({
            error,
            description,
            redirectUri,
            ...(state === undefined ? {} : { state }),
          })
        : new McpOAuthPageError({ description });
    if (request.response_type !== "code") {
      return yield* fail("unsupported_response_type", "Only response_type=code is supported.");
    }
    const codeChallenge = request.code_challenge;
    if (
      request.code_challenge_method !== "S256" ||
      codeChallenge === undefined ||
      !CODE_CHALLENGE_PATTERN.test(codeChallenge)
    ) {
      return yield* fail("invalid_request", "PKCE with code_challenge_method=S256 is required.");
    }
    if (!sameResource(urls, request.resource)) {
      return yield* fail("invalid_target", `This server only issues tokens for ${urls.resource}.`);
    }
    return {
      client,
      redirectUri,
      codeChallenge,
      state,
      resource: urls.resource,
      issuer: urls.issuer,
    } satisfies AuthorizationRequest;
  });

  const mintCode = Effect.fn("McpOAuth.mintCode")(function* (
    authorization: AuthorizationRequest,
    access: AuthMcpClientAccess,
    approvedAtRevocationCount: number,
  ) {
    const now = yield* Clock.currentTimeMillis;
    const code = Buffer.from(yield* random.randomBytes(32)).toString("base64url");
    yield* Ref.update(codes, (current) => {
      const next = new Map(Array.from(current).filter(([, pending]) => pending.expiresAtMs > now));
      next.set(code, {
        clientId: authorization.client.clientId,
        clientName: authorization.client.name,
        peerLink: authorization.client.peerLink,
        redirectUri: authorization.redirectUri,
        codeChallenge: authorization.codeChallenge,
        resource: authorization.resource,
        access,
        approvedAtRevocationCount,
        expiresAtMs: now + AUTHORIZATION_CODE_TTL_MS,
      });
      return next;
    });
    return code;
  });

  const dropCode = (code: string) =>
    Ref.update(codes, (current) => {
      const next = new Map(current);
      next.delete(code);
      return next;
    });

  const approve: McpOAuth["Service"]["approve"] = Effect.fn("McpOAuth.approve")(function* (input) {
    const { decision } = input;
    // An outside agent's sign-in is answered on the consent page, where the user sees it.
    if (!input.authorization.client.peerLink) {
      return yield* new McpOAuthPageError({
        description: "Open the sign-in page in a browser to approve this app.",
      });
    }
    if (!(yield* decisionAllowed)) {
      return yield* new McpOAuthPageError({
        description: "Too many sign-ins answered just now. Try again in a minute.",
      });
    }
    // Read before the code is spent: "revoke all others" then ends this sign-in whether it lands
    // before the check or after the code is handed out.
    const approvedAtRevocationCount = yield* environmentAuth.otherSessionRevocationCount;
    yield* environmentAuth.consumeMcpApprovalCode(decision.code, decision.access).pipe(
      Effect.catchIf(EnvironmentAuth.isServerAuthInternalError, (cause) =>
        Effect.logError("MCP approval code check failed.", { cause }).pipe(
          Effect.andThen(
            Effect.fail(
              new McpOAuthPageError({
                description: "The code could not be checked. Try again.",
              }),
            ),
          ),
        ),
      ),
    );
    const code = yield* mintCode(input.authorization, decision.access, approvedAtRevocationCount);
    yield* Effect.logInfo("Approved an MCP client sign-in.", {
      client: input.authorization.client.name,
      access: decision.access,
    });
    return redirectWith(input.authorization.redirectUri, {
      code,
      state: input.authorization.state,
      iss: input.authorization.issuer,
    });
  });

  const signIns = yield* SubscriptionRef.make<ReadonlyMap<string, SignIn>>(new Map());

  const randomMatchCode = random.randomBytes(6).pipe(
    Effect.map((bytes) => {
      // 32 symbols divide 256 evenly, so the modulo carries no bias.
      const symbols = Array.from(bytes, (byte) => MATCH_CODE_ALPHABET[byte % 32]!).join("");
      return `${symbols.slice(0, 3)}-${symbols.slice(3)}`;
    }),
  );

  const viewOf = (signIn: SignIn): SignInView => ({
    id: signIn.id,
    clientName: signIn.clientName,
    redirectHost: signIn.redirectHost,
    loopbackRedirect: signIn.loopbackRedirect,
    matchCode: signIn.matchCode,
    codeAttemptsLeft: signIn.codeAttemptsLeft,
  });

  const live = (current: ReadonlyMap<string, SignIn>, now: number) =>
    new Map(Array.from(current).filter(([, signIn]) => signIn.expiresAtMs > now));

  /** Drops expired sign-ins, publishing only when one was dropped. */
  const pruneExpired = Clock.currentTimeMillis.pipe(
    Effect.flatMap((now) =>
      SubscriptionRef.updateSome(signIns, (current) => {
        const next = live(current, now);
        return next.size === current.size ? Option.none() : Option.some(next);
      }),
    ),
  );

  const beginSignIn: McpOAuth["Service"]["beginSignIn"] = Effect.fn("McpOAuth.beginSignIn")(
    function* (authorization) {
      const forbiddenBy = yield* environmentAuth.outsideAgentsForbiddenBy;
      if (forbiddenBy !== null) {
        return yield* new McpOAuthPageError({
          description: `${forbiddenBy} does not allow outside agents on this environment.`,
        });
      }
      if (!(yield* signInAllowed)) {
        return yield* new McpOAuthPageError({
          description: "Too many sign-ins started just now. Wait a minute, then try again.",
          busy: true,
        });
      }
      const now = yield* Clock.currentTimeMillis;
      const redirect = new URL(authorization.redirectUri);
      const signIn: SignIn = {
        id: Buffer.from(yield* random.randomBytes(24)).toString("base64url"),
        clientName: authorization.client.name,
        redirectHost: redirect.host,
        loopbackRedirect: LOOPBACK_HOSTNAMES.has(redirect.hostname),
        matchCode: yield* randomMatchCode,
        codeAttemptsLeft: MAX_CODE_ATTEMPTS,
        authorization,
        expiresAtMs: now + SIGN_IN_TTL_MS,
      };
      const added = yield* SubscriptionRef.modify(signIns, (current) => {
        const next = live(current, now);
        if (next.size >= MAX_PENDING_SIGN_INS) return [false, next] as const;
        next.set(signIn.id, signIn);
        return [true, next] as const;
      });
      if (!added) {
        return yield* new McpOAuthPageError({
          description: "Too many sign-ins are waiting. Answer or let them expire, then try again.",
          busy: true,
        });
      }
      yield* Effect.sleep(SIGN_IN_TTL_MS).pipe(
        Effect.andThen(pruneExpired),
        Effect.forkIn(serviceScope),
      );
      yield* Effect.logInfo("An outside agent is waiting to sign in.", {
        client: signIn.clientName,
        redirectHost: signIn.redirectHost,
      });
      return viewOf(signIn);
    },
  );

  const signInPage: McpOAuth["Service"]["signInPage"] = (id) =>
    Clock.currentTimeMillis.pipe(
      Effect.flatMap((now) =>
        SubscriptionRef.modify(signIns, (current): readonly [SignInPage, Map<string, SignIn>] => {
          const next = live(current, now);
          const signIn = next.get(id);
          if (signIn === undefined) return [{ _tag: "missing" }, next];
          if (signIn.redirectTo === undefined) {
            return [{ _tag: "waiting", signIn: viewOf(signIn) }, next];
          }
          next.delete(id);
          return [{ _tag: "finished", redirectTo: signIn.redirectTo }, next];
        }),
      ),
      Effect.withSpan("McpOAuth.signInPage"),
    );

  const peekSignIn: McpOAuth["Service"]["peekSignIn"] = (id) =>
    Effect.all([SubscriptionRef.get(signIns), Clock.currentTimeMillis]).pipe(
      Effect.map(([current, now]): SignInPage => {
        const signIn = current.get(id);
        if (signIn === undefined || signIn.expiresAtMs <= now) return { _tag: "missing" };
        return signIn.redirectTo === undefined
          ? { _tag: "waiting", signIn: viewOf(signIn) }
          : { _tag: "finished", redirectTo: signIn.redirectTo };
      }),
    );

  /** Settles a waiting sign-in once; false when it already finished or expired. */
  const settle = (id: string, redirectTo: string) =>
    Clock.currentTimeMillis.pipe(
      Effect.flatMap((now) =>
        SubscriptionRef.modify(signIns, (current) => {
          const next = live(current, now);
          const signIn = next.get(id);
          if (signIn === undefined || signIn.redirectTo !== undefined)
            return [false, next] as const;
          next.set(id, { ...signIn, redirectTo, checkingCode: false });
          return [true, next] as const;
        }),
      ),
    );

  const waitingSignIn = (id: string) =>
    Effect.all([SubscriptionRef.get(signIns), Clock.currentTimeMillis]).pipe(
      Effect.map(([current, now]) => {
        const signIn = current.get(id);
        return signIn === undefined || signIn.expiresAtMs <= now || signIn.redirectTo !== undefined
          ? undefined
          : signIn;
      }),
    );

  /** False, with its authorization code dropped, when another answer settled it first. */
  const settleApproved = Effect.fnUntraced(function* (
    signIn: SignIn,
    access: AuthMcpClientAccess,
    revocation: number,
  ) {
    const code = yield* mintCode(signIn.authorization, access, revocation);
    const redirectTo = redirectWith(signIn.authorization.redirectUri, {
      code,
      state: signIn.authorization.state,
      iss: signIn.authorization.issuer,
    });
    if (yield* settle(signIn.id, redirectTo)) return true;
    yield* dropCode(code);
    return false;
  });

  const deniedRedirect = (signIn: SignIn) =>
    redirectWith(signIn.authorization.redirectUri, {
      error: "access_denied",
      state: signIn.authorization.state,
      iss: signIn.authorization.issuer,
    });

  const answerSignIn: McpOAuth["Service"]["answerSignIn"] = Effect.fn("McpOAuth.answerSignIn")(
    function* ({ id, answer, grantorScopes }) {
      const signIn = yield* waitingSignIn(id);
      if (signIn === undefined) return yield* new AgentSignInError({ reason: "not_found" });
      if (answer._tag === "approve") yield* checkGrant(answer.access, grantorScopes);
      const settled =
        answer._tag === "deny"
          ? yield* settle(id, deniedRedirect(signIn))
          : yield* settleApproved(
              signIn,
              answer.access,
              yield* environmentAuth.otherSessionRevocationCount,
            );
      if (!settled) return yield* new AgentSignInError({ reason: "not_found" });
      yield* Effect.logInfo("Answered an outside agent's sign-in.", {
        client: signIn.clientName,
        answer: answer._tag,
        ...(answer._tag === "approve" ? { access: answer.access } : {}),
      });
    },
  );

  /** Ends a running code check. A wrong code keeps its try spent; the last try ends the sign-in. */
  const endCodeCheck = (id: string, outcome: "wrong" | "refund") =>
    SubscriptionRef.updateSome(signIns, (current) => {
      const latest = current.get(id);
      if (latest?.checkingCode !== true) return Option.none();
      const ended: SignIn = { ...latest, checkingCode: false };
      const next = new Map(current);
      next.set(
        id,
        outcome === "refund"
          ? { ...ended, codeAttemptsLeft: ended.codeAttemptsLeft + 1 }
          : ended.codeAttemptsLeft > 0 || ended.redirectTo !== undefined
            ? ended
            : { ...ended, redirectTo: deniedRedirect(ended) },
      );
      return Option.some(next);
    });

  const approveSignInWithCode: McpOAuth["Service"]["approveSignInWithCode"] = Effect.fn(
    "McpOAuth.approveSignInWithCode",
  )(function* ({ id, code }) {
    if (!(yield* codeSubmissionAllowed)) {
      return yield* new McpOAuthPageError({
        description: "Too many codes tried just now. Wait a minute, then try again.",
      });
    }
    const now = yield* Clock.currentTimeMillis;
    // One check at a time, its try spent before the code is, so concurrent posts can neither
    // outrun the try limit nor spend two codes on one sign-in.
    const reserved = yield* SubscriptionRef.modifySome(
      signIns,
      (current): readonly [SignIn | "missing" | "busy", Option.Option<Map<string, SignIn>>] => {
        const signIn = current.get(id);
        if (signIn === undefined || signIn.expiresAtMs <= now || signIn.redirectTo !== undefined) {
          return ["missing", Option.none()];
        }
        if (signIn.checkingCode === true) return ["busy", Option.none()];
        const next = new Map(current);
        next.set(id, {
          ...signIn,
          checkingCode: true,
          codeAttemptsLeft: signIn.codeAttemptsLeft - 1,
        });
        return [signIn, Option.some(next)];
      },
    );
    if (reserved === "missing") return yield* peekSignIn(id);
    if (reserved === "busy") {
      return yield* new McpOAuthPageError({
        description: "Another code is being checked for this sign-in.",
        busy: true,
      });
    }
    const signIn = reserved;
    // A check that failed to run gives its try back.
    return yield* Effect.gen(function* () {
      const revocation = yield* environmentAuth.otherSessionRevocationCount;
      const parsed = parseMcpApprovalCode(code);
      const spent =
        parsed === undefined
          ? false
          : yield* environmentAuth
              .consumeMcpApprovalCode(parsed.credential, parsed.access, "agent")
              .pipe(
                Effect.as(true),
                Effect.catchTags({ ServerAuthMcpApprovalCodeError: () => Effect.succeed(false) }),
                Effect.catchIf(EnvironmentAuth.isServerAuthInternalError, (cause) =>
                  Effect.logError("Agent code check failed.", { cause }).pipe(
                    Effect.andThen(
                      Effect.fail(
                        new McpOAuthPageError({
                          description: "The code could not be checked. Try again.",
                        }),
                      ),
                    ),
                  ),
                ),
              );
      if (spent && parsed !== undefined) {
        // Another device answered while the code was checked: its answer stands.
        if (yield* settleApproved(signIn, parsed.access, revocation)) {
          yield* Effect.logInfo("Approved an outside agent's sign-in with a code.", {
            client: signIn.clientName,
            access: parsed.access,
          });
        }
        return yield* peekSignIn(id);
      }
      yield* endCodeCheck(id, "wrong");
      const page = yield* peekSignIn(id);
      if (page._tag !== "waiting") return page;
      return yield* new McpOAuthPageError({
        description:
          page.signIn.codeAttemptsLeft === 1
            ? "That code did not work. One try is left."
            : `That code did not work. ${page.signIn.codeAttemptsLeft} tries are left.`,
      });
    }).pipe(Effect.ensuring(endCodeCheck(id, "refund")));
    // A dropped request must not leave the sign-in marked as checking, or a code spent for nothing.
  }, Effect.uninterruptible);

  const signInRequests = Stream.unwrap(
    pruneExpired.pipe(Effect.as(SubscriptionRef.changes(signIns))),
  ).pipe(
    Stream.mapEffect((current) =>
      Clock.currentTimeMillis.pipe(
        Effect.map((now) =>
          Array.from(current.values())
            .filter((signIn) => signIn.redirectTo === undefined && signIn.expiresAtMs > now)
            .map((signIn): AgentSignInRequest => ({
              id: signIn.id,
              clientName: signIn.clientName,
              redirectHost: signIn.redirectHost,
              loopbackRedirect: signIn.loopbackRedirect,
              matchCode: signIn.matchCode,
              expiresAt: DateTime.makeUnsafe(signIn.expiresAtMs),
            })),
        ),
      ),
    ),
    // A request's fields never change, so the ids say whether the list did.
    Stream.changesWith(
      (left, right) =>
        left.length === right.length && left.every((request, i) => request.id === right[i]!.id),
    ),
  );

  const issueAgentToken: McpOAuth["Service"]["issueAgentToken"] = Effect.fn(
    "McpOAuth.issueAgentToken",
  )(function* ({ label, access, client, grantorScopes }) {
    yield* checkGrant(access, grantorScopes);
    const issued = yield* environmentAuth
      .issueMcpClientSession({ label, access, purpose: "outside-agent", client })
      .pipe(Effect.mapError((cause) => new AgentSignInError({ reason: "issue_failed", cause })));
    yield* Effect.logInfo("Issued an agent token.", { label, access });
    return { token: issued.token, expiresAt: DateTime.toUtc(issued.expiresAt) };
  });

  const exchangeCode: McpOAuth["Service"]["exchangeCode"] = Effect.fn("McpOAuth.exchangeCode")(
    function* ({ request, urls, token }) {
      const fail = (error: AuthMcpTokenError["error"], description: string) =>
        new AuthMcpTokenError({ error, error_description: description });
      if (token.grant_type !== "authorization_code") {
        return yield* fail("unsupported_grant_type", "Only authorization_code is supported.");
      }
      if (!(yield* tokenExchangeAllowed)) {
        return yield* fail("invalid_request", "Too many token requests. Try again in a minute.");
      }
      const { redirect_uri: redirectUri, client_id: clientId } = token;
      const code = token.code && Redacted.value(token.code);
      const verifier = token.code_verifier && Redacted.value(token.code_verifier);
      if (!code || !redirectUri || !clientId || !verifier) {
        return yield* fail(
          "invalid_request",
          "code, redirect_uri, client_id and code_verifier are required.",
        );
      }
      if ((yield* parseClientId(clientId)) === undefined) {
        return yield* fail("invalid_client", "The client is unknown.");
      }
      // A presented code is spent even when a later check fails (RFC 6749 §4.1.2).
      const pending = yield* Ref.modify(codes, (current) => {
        const found = current.get(code);
        if (found === undefined) return [undefined, current] as const;
        const next = new Map(current);
        next.delete(code);
        return [found, next] as const;
      });
      const now = yield* Clock.currentTimeMillis;
      if (
        pending === undefined ||
        pending.expiresAtMs <= now ||
        pending.clientId !== clientId ||
        pending.redirectUri !== redirectUri ||
        pending.resource !== urls.resource ||
        !sameResource(urls, token.resource) ||
        !(yield* pkceVerifies(verifier, pending.codeChallenge))
      ) {
        return yield* fail("invalid_grant", "The authorization code is invalid or expired.");
      }
      const issued = yield* environmentAuth
        .issueMcpClientSession({
          label: pending.clientName,
          access: pending.access,
          purpose: pending.peerLink ? "peer-link" : "outside-agent",
          client: deriveAuthClientMetadata({ request }),
          approvedAtRevocationCount: pending.approvedAtRevocationCount,
        })
        .pipe(
          Effect.catch((cause) =>
            cause._tag === "ServerAuthInvalidCredentialError"
              ? Effect.fail(fail("invalid_grant", "The authorization code is invalid or expired."))
              : Effect.logError("Could not issue an MCP client session.", { cause }).pipe(
                  Effect.andThen(
                    Effect.fail(fail("invalid_grant", "The session could not be issued.")),
                  ),
                ),
          ),
        );
      return {
        access_token: issued.token,
        token_type: "Bearer",
        expires_in: Math.max(0, Math.floor((issued.expiresAt.epochMilliseconds - now) / 1000)),
        scope: encodeOAuthScope(mcpClientScopes(pending.access)),
      };
    },
  );

  return McpOAuth.of({
    beginSignIn,
    signInPage,
    peekSignIn,
    approveSignInWithCode,
    answerSignIn,
    signIns: signInRequests,
    issueAgentToken,
    register,
    validateAuthorization,
    approve,
    exchangeCode,
  });
});

export const layer = Layer.effect(McpOAuth, make);

export const layerMcpClientAuthenticator = Layer.effect(
  McpHttpServer.McpClientAuthenticator,
  Effect.gen(function* () {
    const environmentAuth = yield* EnvironmentAuth.EnvironmentAuth;
    const environment = yield* ServerEnvironment.ServerEnvironment;
    const environmentId = yield* environment.getEnvironmentId;
    return McpHttpServer.McpClientAuthenticator.of({
      authenticate: Effect.fn("McpHttpServer.McpClientAuthenticator.authenticate")(
        function (request) {
          return environmentAuth.authenticateMcpClient(request).pipe(
            Effect.zipWith(
              Clock.currentTimeMillis,
              (client, issuedAt): McpInvocationContext.McpInvocationScope => ({
                environmentId,
                requestNamespace: `client:${client.sessionId}`,
                thread: undefined,
                client: {
                  sessionId: client.sessionId,
                  label: client.label,
                  access: client.access,
                  linked: client.peerLink,
                  linkExpiresAt: client.linkExpiresAt,
                },
                capabilities: new Set<McpInvocationContext.McpCapability>([
                  "orchestration",
                  "worktree",
                  "pull-requests",
                ]),
                issuedAt,
              }),
            ),
            Effect.catch((error) =>
              (EnvironmentAuth.isServerAuthCredentialError(error)
                ? Effect.void
                : Effect.logWarning("MCP client authentication failed.", { cause: error })
              ).pipe(Effect.as(undefined)),
            ),
          );
        },
      ),
    });
  }),
);
