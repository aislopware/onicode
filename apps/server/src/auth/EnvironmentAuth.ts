import {
  AuthAccessTokenType,
  AuthAccessWriteScope,
  AuthOrchestrationOperateScope,
  AuthStandardClientScopes,
  type AuthAccessTokenResult,
  type AuthClientMetadata,
  type AuthClientSession,
  type AuthCreatePairingCredentialInput,
  type AuthEnvironmentScope,
  type AuthMcpClientAccess,
  type AuthPairingLink,
  type AuthPairingCredentialResult,
  type AuthSessionId,
  type AuthSessionState,
  authScopeResponse,
  type ServerAuthDescriptor,
  type AuthWebSocketTicketResult,
  type AuthWebSocketTicketUse,
  mcpClientScopes,
} from "@onicode/contracts";
import { isLocalLoopbackHost } from "@onicode/shared/hostClassification";
import { encodeOAuthScope } from "@onicode/shared/oauthScope";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Redacted from "effect/Redacted";
import * as Schema from "effect/Schema";
import * as HttpServerRequest from "effect/http/HttpServerRequest";

import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as EnvironmentAuthPolicy from "./EnvironmentAuthPolicy.ts";
import * as PairingGrantStore from "./PairingGrantStore.ts";
import * as ServerSecretStore from "./ServerSecretStore.ts";
import * as SessionStore from "./SessionStore.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as ManagedLimits from "../policy/managedLimits.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import { requestUrl } from "../requestUrl.ts";

const DEFAULT_SESSION_SUBJECT = "cli-issued-session";
export const INTERNAL_ADMINISTRATIVE_BOOTSTRAP_SUBJECT = "administrative-bootstrap";

export interface IssuedPairingLink {
  readonly id: string;
  readonly credential: string;
  readonly scopes: ReadonlyArray<AuthEnvironmentScope>;
  readonly subject: string;
  readonly label?: string;
  readonly createdAt: DateTime.Utc;
  readonly expiresAt: DateTime.Utc;
}

export interface IssuedBearerSession {
  readonly sessionId: AuthSessionId;
  readonly token: string;
  readonly scopes: ReadonlyArray<AuthEnvironmentScope>;
  readonly subject: string;
  readonly client: AuthClientMetadata;
  readonly expiresAt: DateTime.Utc;
}

// MCP client tokens are restricted to /mcp; HTTP API and WebSocket RPC would bypass their access cap.
// Peer links carry the signed peer-link claim; outside-agent sessions from before have neither.
const PEER_LINK_SUBJECT = "mcp-client";
const THREAD_CLI_SUBJECT = "thread-cli";
const MCP_CLIENT_SUBJECTS: ReadonlySet<string> = new Set([PEER_LINK_SUBJECT, THREAD_CLI_SUBJECT]);
const MCP_CLIENT_SESSION_TTL = Duration.days(30);

export interface McpClientSession {
  readonly sessionId: AuthSessionId;
  readonly label: string;
  readonly access: AuthMcpClientAccess;
  /** Another Onicode environment's link, whose work here stays within it. */
  readonly peerLink: boolean;
}

export interface AuthenticatedSession {
  readonly sessionId: AuthSessionId;
  readonly subject: string;
  readonly scopes: ReadonlyArray<AuthEnvironmentScope>;
  readonly expiresAt?: DateTime.DateTime;
}

const serverAuthInternalErrorContext = {
  cause: Schema.Defect(),
};

export class ServerAuthBootstrapCredentialValidationError extends Schema.TaggedError<ServerAuthBootstrapCredentialValidationError>()(
  "ServerAuthBootstrapCredentialValidationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to validate bootstrap credential.";
  }
}

export class ServerAuthSessionCredentialValidationError extends Schema.TaggedError<ServerAuthSessionCredentialValidationError>()(
  "ServerAuthSessionCredentialValidationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to validate session credential.";
  }
}

export class ServerAuthAuthenticatedAccessTokenIssueError extends Schema.TaggedError<ServerAuthAuthenticatedAccessTokenIssueError>()(
  "ServerAuthAuthenticatedAccessTokenIssueError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to issue authenticated access token.";
  }
}

export class ServerAuthPairingLinkCreationError extends Schema.TaggedError<ServerAuthPairingLinkCreationError>()(
  "ServerAuthPairingLinkCreationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to create pairing link.";
  }
}

export class ServerAuthPairingLinksListError extends Schema.TaggedError<ServerAuthPairingLinksListError>()(
  "ServerAuthPairingLinksListError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to list pairing links.";
  }
}

export class ServerAuthPairingLinkRevocationError extends Schema.TaggedError<ServerAuthPairingLinkRevocationError>()(
  "ServerAuthPairingLinkRevocationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to revoke pairing link.";
  }
}

export class ServerAuthSessionTokenIssueError extends Schema.TaggedError<ServerAuthSessionTokenIssueError>()(
  "ServerAuthSessionTokenIssueError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to issue session token.";
  }
}

export class ServerAuthSessionsListError extends Schema.TaggedError<ServerAuthSessionsListError>()(
  "ServerAuthSessionsListError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to list sessions.";
  }
}

export class ServerAuthSessionRevocationError extends Schema.TaggedError<ServerAuthSessionRevocationError>()(
  "ServerAuthSessionRevocationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to revoke session.";
  }
}

export class ServerAuthOtherSessionsRevocationError extends Schema.TaggedError<ServerAuthOtherSessionsRevocationError>()(
  "ServerAuthOtherSessionsRevocationError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to revoke other sessions.";
  }
}

export class ServerAuthWebSocketTokenIssueError extends Schema.TaggedError<ServerAuthWebSocketTokenIssueError>()(
  "ServerAuthWebSocketTokenIssueError",
  {
    ...serverAuthInternalErrorContext,
  },
) {
  override get message(): string {
    return "Failed to issue websocket token.";
  }
}

export const ServerAuthInternalError = Schema.Union([
  ServerAuthBootstrapCredentialValidationError,
  ServerAuthSessionCredentialValidationError,
  ServerAuthAuthenticatedAccessTokenIssueError,
  ServerAuthPairingLinkCreationError,
  ServerAuthPairingLinksListError,
  ServerAuthPairingLinkRevocationError,
  ServerAuthSessionTokenIssueError,
  ServerAuthSessionsListError,
  ServerAuthSessionRevocationError,
  ServerAuthOtherSessionsRevocationError,
  ServerAuthWebSocketTokenIssueError,
]);
export type ServerAuthInternalError = typeof ServerAuthInternalError.Type;
export const isServerAuthInternalError = Schema.is(ServerAuthInternalError);

export class ServerAuthMissingCredentialError extends Schema.TaggedError<ServerAuthMissingCredentialError>()(
  "ServerAuthMissingCredentialError",
  {},
) {
  override get message(): string {
    return "Server authentication credential is missing.";
  }
}

export class ServerAuthInvalidCredentialError extends Schema.TaggedError<ServerAuthInvalidCredentialError>()(
  "ServerAuthInvalidCredentialError",
  {
    diagnostic: Schema.optional(Schema.String),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return "Server authentication credential is invalid.";
  }
}

export const ServerAuthCredentialError = Schema.Union([
  ServerAuthMissingCredentialError,
  ServerAuthInvalidCredentialError,
]);
export type ServerAuthCredentialError = typeof ServerAuthCredentialError.Type;
export const isServerAuthCredentialError = Schema.is(ServerAuthCredentialError);
export const serverAuthCredentialReason = (
  error: ServerAuthCredentialError,
): "missing_credential" | "invalid_credential" =>
  error._tag === "ServerAuthMissingCredentialError" ? "missing_credential" : "invalid_credential";

export class ServerAuthInvalidScopeError extends Schema.TaggedError<ServerAuthInvalidScopeError>()(
  "ServerAuthInvalidScopeError",
  {},
) {
  override get message(): string {
    return "The requested authentication scope is invalid.";
  }
}

export class ServerAuthScopeNotGrantedError extends Schema.TaggedError<ServerAuthScopeNotGrantedError>()(
  "ServerAuthScopeNotGrantedError",
  {},
) {
  override get message(): string {
    return "The requested authentication scope was not granted.";
  }
}

export const ServerAuthInvalidRequestError = Schema.Union([
  ServerAuthInvalidScopeError,
  ServerAuthScopeNotGrantedError,
]);
export type ServerAuthInvalidRequestError = typeof ServerAuthInvalidRequestError.Type;
export const isServerAuthInvalidRequestError = Schema.is(ServerAuthInvalidRequestError);
export const serverAuthInvalidRequestReason = (
  error: ServerAuthInvalidRequestError,
): "invalid_scope" | "scope_not_granted" =>
  error._tag === "ServerAuthInvalidScopeError" ? "invalid_scope" : "scope_not_granted";

export class ServerAuthMcpApprovalCodeError extends Schema.TaggedError<ServerAuthMcpApprovalCodeError>()(
  "ServerAuthMcpApprovalCodeError",
  { reason: Schema.Literals(["unknown_or_used", "not_a_pairing_code", "insufficient_scope"]) },
) {
  override get message(): string {
    return this.reason === "insufficient_scope"
      ? "That pairing code cannot grant this access. It has not been used, so choose an access level it grants, or create a code with the standard scopes."
      : this.reason === "not_a_pairing_code"
        ? "That is not a one-time pairing code."
        : "That pairing code is unknown, expired, or already used.";
  }
}

export class ServerAuthForbiddenOperationError extends Schema.TaggedError<ServerAuthForbiddenOperationError>()(
  "ServerAuthForbiddenOperationError",
  {},
) {
  override get message(): string {
    return "The current authentication session cannot revoke itself.";
  }
}

export class EnvironmentAuth extends Context.Service<
  EnvironmentAuth,
  {
    readonly getDescriptor: () => Effect.Effect<ServerAuthDescriptor>;
    readonly getSessionState: (
      request: HttpServerRequest.HttpServerRequest,
    ) => Effect.Effect<AuthSessionState, ServerAuthInternalError>;
    readonly exchangeBootstrapCredentialForAccessToken: (
      credential: string,
      requestedScopes: ReadonlyArray<AuthEnvironmentScope> | undefined,
      requestMetadata: AuthClientMetadata,
    ) => Effect.Effect<
      AuthAccessTokenResult,
      ServerAuthInvalidCredentialError | ServerAuthInvalidRequestError | ServerAuthInternalError
    >;
    readonly createPairingLink: (input?: {
      readonly ttl?: Duration.Duration;
      readonly label?: string;
      readonly scopes?: ReadonlyArray<AuthEnvironmentScope>;
      readonly subject?: string;
      readonly purpose?: "startup";
    }) => Effect.Effect<
      IssuedPairingLink,
      ServerAuthInternalError | ManagedLimits.ManagedPolicyRefusedError
    >;
    readonly issuePairingCredential: (
      input?: AuthCreatePairingCredentialInput,
    ) => Effect.Effect<
      AuthPairingCredentialResult,
      ServerAuthInternalError | ManagedLimits.ManagedPolicyRefusedError
    >;
    readonly issueStartupPairingCredential: () => Effect.Effect<
      AuthPairingCredentialResult,
      ServerAuthInternalError
    >;
    readonly listPairingLinks: (input?: {
      readonly excludeSubjects?: ReadonlyArray<string>;
    }) => Effect.Effect<ReadonlyArray<AuthPairingLink>, ServerAuthInternalError>;
    readonly revokePairingLink: (id: string) => Effect.Effect<boolean, ServerAuthInternalError>;
    readonly issueSession: (input?: {
      readonly ttl?: Duration.Duration;
      readonly subject?: string;
      readonly scopes?: ReadonlyArray<AuthEnvironmentScope>;
      readonly label?: string;
    }) => Effect.Effect<IssuedBearerSession, ServerAuthInternalError>;
    readonly listSessions: () => Effect.Effect<
      ReadonlyArray<AuthClientSession>,
      ServerAuthInternalError
    >;
    readonly revokeSession: (
      sessionId: AuthSessionId,
    ) => Effect.Effect<boolean, ServerAuthInternalError>;
    readonly listClientSessions: (
      currentSessionId: AuthSessionId,
    ) => Effect.Effect<ReadonlyArray<AuthClientSession>, ServerAuthInternalError>;
    readonly revokeClientSession: (
      currentSessionId: AuthSessionId,
      targetSessionId: AuthSessionId,
    ) => Effect.Effect<boolean, ServerAuthForbiddenOperationError | ServerAuthInternalError>;
    readonly revokeOtherClientSessions: (
      currentSessionId: AuthSessionId,
    ) => Effect.Effect<number, ServerAuthInternalError>;
    readonly authenticateHttpRequest: (
      request: HttpServerRequest.HttpServerRequest,
    ) => Effect.Effect<AuthenticatedSession, ServerAuthCredentialError | ServerAuthInternalError>;
    readonly authenticateWebSocketUpgrade: (
      request: HttpServerRequest.HttpServerRequest,
      use: AuthWebSocketTicketUse,
    ) => Effect.Effect<AuthenticatedSession, ServerAuthCredentialError | ServerAuthInternalError>;
    readonly issueWebSocketTicket: (
      session: Pick<AuthenticatedSession, "sessionId">,
      input?: { readonly use?: AuthWebSocketTicketUse },
    ) => Effect.Effect<AuthWebSocketTicketResult, ServerAuthInternalError>;
    readonly authenticateMcpClient: (
      request: HttpServerRequest.HttpServerRequest,
    ) => Effect.Effect<McpClientSession, ServerAuthCredentialError | ServerAuthInternalError>;
    readonly issueMcpClientSession: (input: {
      readonly label: string;
      readonly access: AuthMcpClientAccess;
      readonly purpose: "peer-link" | "thread-cli";
      readonly client: AuthClientMetadata;
    }) => Effect.Effect<
      {
        readonly sessionId: AuthSessionId;
        readonly token: string;
        readonly expiresAt: DateTime.DateTime;
      },
      ServerAuthInternalError
    >;
    /** Spends a code covering all granted scopes. Rejects desktop grants and leaves refused proof-bound codes unspent. */
    readonly consumeMcpApprovalCode: (
      code: string,
      access: AuthMcpClientAccess,
    ) => Effect.Effect<void, ServerAuthMcpApprovalCodeError | ServerAuthInternalError>;
  }
>()("onicode/auth/EnvironmentAuth") {}

const AUTHORIZATION_PREFIX = "Bearer ";
const WebSocketUpgradeQuery = Schema.Struct({
  wsTicket: Schema.optionalKey(Schema.RedactedFromValue(Schema.String)),
});
const decodeWebSocketUpgradeQuery = Schema.decodeEffect(WebSocketUpgradeQuery);

const bySessionPriority = (left: AuthClientSession, right: AuthClientSession) => {
  const leftCanManage = left.scopes.includes(AuthAccessWriteScope);
  const rightCanManage = right.scopes.includes(AuthAccessWriteScope);
  if (leftCanManage !== rightCanManage) {
    return leftCanManage ? -1 : 1;
  }
  if (left.connected !== right.connected) {
    return left.connected ? -1 : 1;
  }
  return right.issuedAt.epochMilliseconds - left.issuedAt.epochMilliseconds;
};

function toBootstrapExchangeError(
  cause: PairingGrantStore.BootstrapCredentialError,
): ServerAuthInvalidCredentialError | ServerAuthInternalError {
  if (PairingGrantStore.isBootstrapCredentialInternalError(cause)) {
    return new ServerAuthBootstrapCredentialValidationError({ cause });
  }

  return new ServerAuthInvalidCredentialError({
    cause,
  });
}

const rejectMcpClientAudience = <S extends { readonly subject: string }>(session: S) =>
  MCP_CLIENT_SUBJECTS.has(session.subject)
    ? Effect.fail(
        new ServerAuthInvalidCredentialError({
          diagnostic: "MCP client sessions are only accepted by the MCP endpoint.",
        }),
      ).pipe(Effect.tap(() => Effect.logWarning("Rejected an MCP client session outside /mcp.")))
    : Effect.succeed(session);

const mapSessionVerificationErrors = <A, R>(
  effect: Effect.Effect<A, SessionStore.SessionCredentialError, R>,
): Effect.Effect<A, ServerAuthInvalidCredentialError | ServerAuthInternalError, R> =>
  effect.pipe(
    Effect.mapError((cause) =>
      SessionStore.isSessionCredentialInvalidError(cause)
        ? new ServerAuthInvalidCredentialError({ cause })
        : new ServerAuthSessionCredentialValidationError({ cause }),
    ),
  );

export function parseBearerToken(request: HttpServerRequest.HttpServerRequest): string | null {
  const header = request.headers["authorization"];
  if (typeof header !== "string" || !/^Bearer /i.test(header)) {
    return null;
  }
  const token = header.slice(AUTHORIZATION_PREFIX.length).trim();
  return token.length > 0 ? token : null;
}

/** @public */
export const make = Effect.gen(function* () {
  const policy = yield* EnvironmentAuthPolicy.EnvironmentAuthPolicy;
  const bootstrapCredentials = yield* PairingGrantStore.PairingGrantStore;
  const sessions = yield* SessionStore.SessionStore;
  const descriptor = yield* policy.getDescriptor();
  const managedPolicy = yield* ManagedPolicy.ManagedPolicy;

  const authenticateToken = (
    token: string,
  ): Effect.Effect<
    AuthenticatedSession,
    ServerAuthInvalidCredentialError | ServerAuthInternalError
  > =>
    sessions.verify(token).pipe(
      Effect.tapError((cause) =>
        SessionStore.isSessionCredentialInvalidError(cause)
          ? Effect.logWarning("Rejected authenticated session credential.").pipe(
              Effect.annotateLogs({
                reason: cause.message,
              }),
            )
          : Effect.void,
      ),
      mapSessionVerificationErrors,
      Effect.flatMap(rejectMcpClientAudience),
      Effect.map((session) => ({
        sessionId: session.sessionId,
        subject: session.subject,
        scopes: session.scopes,
        ...(session.expiresAt ? { expiresAt: session.expiresAt } : {}),
      })),
    );

  const authenticateRequest = (
    request: HttpServerRequest.HttpServerRequest,
  ): Effect.Effect<AuthenticatedSession, ServerAuthCredentialError | ServerAuthInternalError> => {
    const token = parseBearerToken(request);
    return token === null
      ? Effect.fail(new ServerAuthMissingCredentialError({}))
      : authenticateToken(token);
  };

  const getSessionState: EnvironmentAuth["Service"]["getSessionState"] = (request) =>
    authenticateRequest(request).pipe(
      Effect.map(
        (session) =>
          ({
            authenticated: true,
            auth: descriptor,
            ...authScopeResponse(session.scopes),
            ...(session.expiresAt ? { expiresAt: DateTime.toUtc(session.expiresAt) } : {}),
          }) satisfies AuthSessionState,
      ),
      Effect.catchIf(isServerAuthCredentialError, () =>
        Effect.succeed({
          authenticated: false,
          auth: descriptor,
        } satisfies AuthSessionState),
      ),
      Effect.withSpan("EnvironmentAuth.getSessionState"),
    );

  const consumeBootstrapGrant = (
    credential: string,
    input?: {
      readonly requestedScopes?: ReadonlyArray<AuthEnvironmentScope>;
      readonly requiredScopes?: ReadonlyArray<AuthEnvironmentScope>;
    },
  ) =>
    bootstrapCredentials
      .consume(credential, input)
      .pipe(
        Effect.mapError((cause) =>
          cause._tag === "BootstrapCredentialScopeNotGrantedError"
            ? new ServerAuthScopeNotGrantedError({})
            : toBootstrapExchangeError(cause),
        ),
      );

  const exchangeBootstrapCredentialForAccessToken: EnvironmentAuth["Service"]["exchangeBootstrapCredentialForAccessToken"] =
    (credential, requestedScopes, requestMetadata) => {
      return consumeBootstrapGrant(
        credential,
        requestedScopes !== undefined ? { requestedScopes } : undefined,
      ).pipe(
        Effect.flatMap((grant) =>
          Effect.gen(function* () {
            const grantedScopes =
              requestedScopes === undefined
                ? grant.scopes
                : [...new Set(requestedScopes)].filter((scope) => grant.scopes.includes(scope));
            if (grantedScopes.length === 0) {
              return yield* new ServerAuthScopeNotGrantedError({});
            }
            return yield* sessions
              .issue({
                subject: grant.subject,
                scopes: grantedScopes,
                replaceActiveForSubject: grant.method === "desktop-bootstrap",
                client: {
                  ...requestMetadata,
                  ...(grant.label ? { label: grant.label } : {}),
                },
              })
              .pipe(
                Effect.mapError(
                  (cause) => new ServerAuthAuthenticatedAccessTokenIssueError({ cause }),
                ),
              );
          }),
        ),
        Effect.flatMap((session) =>
          DateTime.now.pipe(
            Effect.map(
              (now) =>
                ({
                  access_token: session.token,
                  issued_token_type: AuthAccessTokenType,
                  token_type: "Bearer",
                  expires_in: Math.max(
                    0,
                    Math.floor(
                      (session.expiresAt.epochMilliseconds - now.epochMilliseconds) / 1000,
                    ),
                  ),
                  scope: encodeOAuthScope(session.scopes),
                }) satisfies AuthAccessTokenResult,
            ),
          ),
        ),
        Effect.withSpan("EnvironmentAuth.exchangeBootstrapCredentialForAccessToken"),
      );
    };

  const toPairingCredential = (issued: IssuedPairingLink): AuthPairingCredentialResult => ({
    id: issued.id,
    credential: issued.credential,
    ...(issued.label ? { label: issued.label } : {}),
    expiresAt: issued.expiresAt,
  });

  const issuePairingLink = Effect.fn("EnvironmentAuth.issuePairingLink")(
    function* (input: Parameters<EnvironmentAuth["Service"]["createPairingLink"]>[0]) {
      const createdAt = yield* DateTime.now;
      const issued = yield* bootstrapCredentials.issueOneTimeToken({
        scopes: input?.scopes ?? AuthStandardClientScopes,
        subject: input?.subject ?? "one-time-token",
        ...(input?.ttl ? { ttl: input.ttl } : {}),
        ...(input?.label ? { label: input.label } : {}),
        ...(input?.purpose ? { purpose: input.purpose } : {}),
      });
      return {
        id: issued.id,
        credential: issued.credential,
        scopes: input?.scopes ?? AuthStandardClientScopes,
        subject: input?.subject ?? "one-time-token",
        ...(issued.label ? { label: issued.label } : {}),
        createdAt: DateTime.toUtc(createdAt),
        expiresAt: DateTime.toUtc(issued.expiresAt),
      } satisfies IssuedPairingLink;
    },
    Effect.mapError((cause) => new ServerAuthPairingLinkCreationError({ cause })),
  );

  const createPairingLink: EnvironmentAuth["Service"]["createPairingLink"] = Effect.fn(
    "EnvironmentAuth.createPairingLink",
  )(function* (input) {
    // The startup link is for a client on this machine; every other link lets a device pair.
    if (input?.purpose !== "startup" && !ManagedLimits.allowsRemoteClients(managedPolicy)) {
      return yield* ManagedLimits.refuseRemoteClients(
        managedPolicy,
        "so this server cannot create pairing links",
      );
    }
    return yield* issuePairingLink(input);
  });

  const listPairingLinks: EnvironmentAuth["Service"]["listPairingLinks"] = (input) =>
    bootstrapCredentials.listActive().pipe(
      Effect.map((pairingLinks) => {
        const excludedSubjects = input?.excludeSubjects ?? [
          INTERNAL_ADMINISTRATIVE_BOOTSTRAP_SUBJECT,
        ];
        return pairingLinks
          .filter((pairingLink) => !excludedSubjects.includes(pairingLink.subject))
          .map((link) => ({ ...link, ...authScopeResponse(link.scopes) }))
          .toSorted(
            (left, right) => right.createdAt.epochMilliseconds - left.createdAt.epochMilliseconds,
          );
      }),
      Effect.mapError((cause) => new ServerAuthPairingLinksListError({ cause })),
      Effect.withSpan("EnvironmentAuth.listPairingLinks"),
    );

  const revokePairingLink: EnvironmentAuth["Service"]["revokePairingLink"] = (id) =>
    bootstrapCredentials.revoke(id).pipe(
      Effect.mapError((cause) => new ServerAuthPairingLinkRevocationError({ cause })),
      Effect.withSpan("EnvironmentAuth.revokePairingLink"),
    );

  const issueSession: EnvironmentAuth["Service"]["issueSession"] = (input) =>
    sessions
      .issue({
        subject: input?.subject ?? DEFAULT_SESSION_SUBJECT,
        scopes: input?.scopes ?? AuthStandardClientScopes,
        client: {
          ...(input?.label ? { label: input.label } : {}),
          deviceType: "bot",
        },
        ...(input?.ttl ? { ttl: input.ttl } : {}),
      })
      .pipe(
        Effect.map(
          (issued) =>
            ({
              sessionId: issued.sessionId,
              token: issued.token,
              scopes: issued.scopes,
              subject: input?.subject ?? DEFAULT_SESSION_SUBJECT,
              client: issued.client,
              expiresAt: DateTime.toUtc(issued.expiresAt),
            }) satisfies IssuedBearerSession,
        ),
        Effect.mapError((cause) => new ServerAuthSessionTokenIssueError({ cause })),
        Effect.withSpan("EnvironmentAuth.issueSession"),
      );

  const listSessions: EnvironmentAuth["Service"]["listSessions"] = () =>
    sessions.listActive().pipe(
      Effect.map((activeSessions) => activeSessions.toSorted(bySessionPriority)),
      Effect.mapError((cause) => new ServerAuthSessionsListError({ cause })),
      Effect.withSpan("EnvironmentAuth.listSessions"),
    );

  const revokeSession: EnvironmentAuth["Service"]["revokeSession"] = (sessionId) =>
    sessions.revoke(sessionId).pipe(
      Effect.mapError((cause) => new ServerAuthSessionRevocationError({ cause })),
      Effect.withSpan("EnvironmentAuth.revokeSession"),
    );

  const revokeOtherSessionsExcept = (sessionId: AuthSessionId) =>
    sessions.revokeAllExcept(sessionId).pipe(
      Effect.mapError((cause) => new ServerAuthOtherSessionsRevocationError({ cause })),
      Effect.withSpan("EnvironmentAuth.revokeOtherSessionsExcept"),
    );

  const issuePairingCredential: EnvironmentAuth["Service"]["issuePairingCredential"] = (input) =>
    createPairingLink({
      scopes: input?.scopes ?? AuthStandardClientScopes,
      subject: "one-time-token",
      ...(input?.label ? { label: input.label } : {}),
    }).pipe(
      Effect.map(toPairingCredential),
      Effect.withSpan("EnvironmentAuth.issuePairingCredential"),
    );

  const issueStartupPairingCredential: EnvironmentAuth["Service"]["issueStartupPairingCredential"] =
    () =>
      // Unguarded: startup already refused auth policies the managed policy leaves out.
      issuePairingLink({
        scopes: AuthStandardClientScopes,
        subject: INTERNAL_ADMINISTRATIVE_BOOTSTRAP_SUBJECT,
        purpose: "startup",
      }).pipe(
        Effect.map(toPairingCredential),
        Effect.withSpan("EnvironmentAuth.issueStartupPairingCredential"),
      );

  const listClientSessions: EnvironmentAuth["Service"]["listClientSessions"] = (currentSessionId) =>
    listSessions().pipe(
      Effect.map((clientSessions) =>
        clientSessions.map((clientSession): AuthClientSession => ({
          ...clientSession,
          ...authScopeResponse(clientSession.scopes),
          current: clientSession.sessionId === currentSessionId,
        })),
      ),
      Effect.withSpan("EnvironmentAuth.listClientSessions"),
    );

  const revokeClientSession: EnvironmentAuth["Service"]["revokeClientSession"] = Effect.fn(
    "EnvironmentAuth.revokeClientSession",
  )(function* (currentSessionId, targetSessionId) {
    if (currentSessionId === targetSessionId) {
      return yield* new ServerAuthForbiddenOperationError({});
    }
    return yield* revokeSession(targetSessionId);
  });

  const revokeOtherClientSessions: EnvironmentAuth["Service"]["revokeOtherClientSessions"] = (
    currentSessionId,
  ) =>
    revokeOtherSessionsExcept(currentSessionId).pipe(
      Effect.withSpan("EnvironmentAuth.revokeOtherClientSessions"),
    );

  const issueWebSocketTicket: EnvironmentAuth["Service"]["issueWebSocketTicket"] = (
    session,
    input,
  ) =>
    sessions.issueWebSocketToken(session.sessionId, input?.use ? { audience: input.use } : {}).pipe(
      Effect.mapError((cause) => new ServerAuthWebSocketTokenIssueError({ cause })),
      Effect.map(
        (issued) =>
          ({
            ticket: Redacted.make(issued.token),
            expiresAt: DateTime.toUtc(issued.expiresAt),
          }) satisfies AuthWebSocketTicketResult,
      ),
      Effect.withSpan("EnvironmentAuth.issueWebSocketTicket"),
    );

  const authenticateHttpRequest: EnvironmentAuth["Service"]["authenticateHttpRequest"] = (
    request,
  ) =>
    authenticateRequest(request).pipe(Effect.withSpan("EnvironmentAuth.authenticateHttpRequest"));

  const authenticateWebSocketUpgrade: EnvironmentAuth["Service"]["authenticateWebSocketUpgrade"] =
    Effect.fn("EnvironmentAuth.authenticateWebSocketUpgrade")(function* (request, use) {
      const url = requestUrl(request);
      if (Option.isSome(url)) {
        const query = yield* decodeWebSocketUpgradeQuery(
          Object.fromEntries(url.value.searchParams),
        ).pipe(Effect.orDie);
        const websocketTicket = query.wsTicket && Redacted.value(query.wsTicket);
        if (websocketTicket && websocketTicket.trim().length > 0) {
          const session = yield* sessions
            .verifyWebSocketToken(websocketTicket)
            .pipe(mapSessionVerificationErrors, Effect.flatMap(rejectMcpClientAudience));
          if (session.audience !== undefined && session.audience !== use) {
            return yield* new ServerAuthInvalidCredentialError({
              diagnostic: `WebSocket ticket was issued for the ${session.audience} route.`,
            });
          }
          return {
            sessionId: session.sessionId,
            subject: session.subject,
            scopes: session.scopes,
            ...(session.expiresAt ? { expiresAt: session.expiresAt } : {}),
          };
        }
      }

      return yield* authenticateRequest(request);
    });

  const authenticateMcpClient: EnvironmentAuth["Service"]["authenticateMcpClient"] = (request) => {
    const token = parseBearerToken(request);
    if (token === null) return Effect.fail(new ServerAuthMissingCredentialError({}));
    return sessions.verify(token).pipe(
      mapSessionVerificationErrors,
      Effect.flatMap((session) =>
        (session.subject === PEER_LINK_SUBJECT && session.peerLink === true) ||
        session.subject === THREAD_CLI_SUBJECT
          ? Effect.succeed({
              sessionId: session.sessionId,
              label: session.client.label ?? "MCP client",
              access: session.scopes.includes(AuthOrchestrationOperateScope)
                ? (session.runtimeModeCeiling ?? "approval-required")
                : "read-only",
              peerLink: session.peerLink === true,
            } satisfies McpClientSession)
          : Effect.fail(
              new ServerAuthInvalidCredentialError({
                diagnostic: "Only linked environments and the thread CLI are accepted here.",
              }),
            ),
      ),
      Effect.withSpan("EnvironmentAuth.authenticateMcpClient"),
    );
  };

  const issueMcpClientSession: EnvironmentAuth["Service"]["issueMcpClientSession"] = (input) =>
    sessions
      .issue({
        subject: input.purpose === "peer-link" ? PEER_LINK_SUBJECT : THREAD_CLI_SUBJECT,
        scopes: mcpClientScopes(input.access),
        ttl: MCP_CLIENT_SESSION_TTL,
        ...(input.access === "read-only" ? {} : { runtimeModeCeiling: input.access }),
        peerLink: input.purpose === "peer-link",
        client: { ...input.client, label: input.label, deviceType: "bot" },
      })
      .pipe(
        Effect.map((issued) => ({
          sessionId: issued.sessionId,
          token: issued.token,
          expiresAt: issued.expiresAt,
        })),
        Effect.mapError((cause) => new ServerAuthSessionTokenIssueError({ cause })),
        Effect.withSpan("EnvironmentAuth.issueMcpClientSession"),
      );

  const consumeMcpApprovalCode: EnvironmentAuth["Service"]["consumeMcpApprovalCode"] = (
    code,
    access,
  ) =>
    consumeBootstrapGrant(code.trim(), { requiredScopes: mcpClientScopes(access) }).pipe(
      Effect.catchTags({
        ServerAuthInvalidCredentialError: () =>
          Effect.fail(new ServerAuthMcpApprovalCodeError({ reason: "unknown_or_used" })),
        ServerAuthScopeNotGrantedError: () =>
          Effect.fail(new ServerAuthMcpApprovalCodeError({ reason: "insufficient_scope" })),
      }),
      Effect.flatMap((grant) =>
        grant.method !== "one-time-token"
          ? Effect.fail(new ServerAuthMcpApprovalCodeError({ reason: "not_a_pairing_code" }))
          : Effect.void,
      ),
      Effect.withSpan("EnvironmentAuth.consumeMcpApprovalCode"),
    );

  yield* revokePairedClientsThePolicyDisallows(managedPolicy, {
    sessions,
    pairingLinks: bootstrapCredentials,
  });

  return EnvironmentAuth.of({
    getDescriptor: Effect.fn("EnvironmentAuth.getDescriptor")(function () {
      return Effect.succeed(descriptor);
    }),
    getSessionState,
    exchangeBootstrapCredentialForAccessToken,
    createPairingLink,
    issuePairingCredential,
    issueStartupPairingCredential,
    listPairingLinks,
    revokePairingLink,
    issueSession,
    listSessions,
    revokeSession,
    listClientSessions,
    revokeClientSession,
    revokeOtherClientSessions,
    authenticateHttpRequest,
    authenticateWebSocketUpgrade,
    issueWebSocketTicket,
    authenticateMcpClient,
    issueMcpClientSession,
    consumeMcpApprovalCode,
  });
});

const LINK_PAIRED_SUBJECT = "one-time-token";

/**
 * Revokes sessions and unused links from pairing the policy no longer allows, so a policy rolled
 * out to a machine that already had phones paired takes effect on the next start.
 */
const revokePairedClientsThePolicyDisallows = Effect.fn(
  "EnvironmentAuth.revokePairedClientsThePolicyDisallows",
)(function* (
  policy: ManagedPolicy.ManagedPolicy["Service"],
  stores: {
    readonly sessions: SessionStore.SessionStore["Service"];
    readonly pairingLinks: PairingGrantStore.PairingGrantStore["Service"];
  },
) {
  if (ManagedLimits.allowsRemoteClients(policy)) return;
  const sessions = (yield* stores.sessions.listActive()).filter(
    (session) =>
      session.subject === LINK_PAIRED_SUBJECT ||
      // The startup link also pairs clients on this machine; only a device elsewhere goes.
      (session.subject === INTERNAL_ADMINISTRATIVE_BOOTSTRAP_SUBJECT &&
        session.client.ipAddress !== undefined &&
        !isLocalLoopbackHost(session.client.ipAddress)),
  );
  const links = (yield* stores.pairingLinks.listActive()).filter(
    (link) => link.subject === LINK_PAIRED_SUBJECT,
  );
  yield* Effect.forEach(sessions, (session) => stores.sessions.revoke(session.sessionId), {
    discard: true,
  });
  yield* Effect.forEach(links, (link) => stores.pairingLinks.revoke(link.id), { discard: true });
  if (sessions.length > 0 || links.length > 0) {
    yield* Effect.logInfo("Revoked clients the managed policy does not allow.", {
      sessions: sessions.length,
      pairingLinks: links.length,
    });
  }
});

export const layer = Layer.effect(EnvironmentAuth, make).pipe(
  Layer.provideMerge(PairingGrantStore.layer),
  Layer.provideMerge(SessionStore.layer),
  Layer.provideMerge(EnvironmentAuthPolicy.layer),
);

const layerStorage = Layer.mergeAll(ServerSecretStore.layer, SqlitePersistence.layerConfig);

export const layerRuntime = layer.pipe(
  Layer.provideMerge(ManagedPolicy.layer),
  Layer.provideMerge(layerStorage),
  Layer.provideMerge(ServerEnvironment.layerIdentity),
);
