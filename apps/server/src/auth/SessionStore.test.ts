import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Deferred from "effect/Deferred";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as TestClock from "effect/testing/TestClock";
import * as SqlClient from "effect/sql/SqlClient";

import * as ServerConfig from "../config.ts";

import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as SessionStore from "./SessionStore.ts";
import * as ServerSecretStore from "./ServerSecretStore.ts";
import { base64UrlDecodeUtf8, base64UrlEncode } from "./utils.ts";
import * as PayloadSigner from "./PayloadSigner.ts";

const layerServerConfig = ServerConfig.layerTest(process.cwd(), {
  prefix: "oni-auth-session-test-",
});

const layerSessionStore = () =>
  SessionStore.layer.pipe(
    Layer.provide(SqlitePersistence.layerMemory),
    Layer.provide(ServerSecretStore.layer),
    Layer.provide(layerServerConfig),
  );

const clientSessionInput = {
  subject: "remote-client",
  ttl: Duration.hours(1),
  client: { label: "Remote desktop", deviceType: "desktop" },
} as const;

it.layer(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))("SessionStore.layer", (it) => {
  it.effect.each(["revoke", "revokeAllExcept", "replace"] as const)(
    "invalidates every connection for a session on %s and leaves other sessions active",
    (operation) =>
      Effect.gen(function* () {
        const sessions = yield* SessionStore.SessionStore;
        const owner = yield* sessions.issue({ subject: "owner" });
        const client = yield* sessions.issue(clientSessionInput);
        const ownerInvalidated = yield* Deferred.make<void>();
        yield* sessions
          .awaitInvalidation(owner.sessionId)
          .pipe(
            Effect.andThen(Deferred.succeed(ownerInvalidated, undefined)),
            Effect.forkScoped({ startImmediately: true }),
          );
        const first = yield* sessions
          .awaitInvalidation(client.sessionId)
          .pipe(Effect.forkScoped({ startImmediately: true }));
        const second = yield* sessions
          .awaitInvalidation(client.sessionId)
          .pipe(Effect.forkScoped({ startImmediately: true }));
        if (operation === "revoke") yield* sessions.revoke(client.sessionId);
        else if (operation === "revokeAllExcept") yield* sessions.revokeAllExcept(owner.sessionId);
        else yield* sessions.issue({ ...clientSessionInput, replaceActiveForSubject: true });
        yield* Fiber.join(first);
        yield* Fiber.join(second);
        expect(yield* Deferred.isDone(ownerInvalidated)).toBe(false);
        yield* sessions.awaitInvalidation(client.sessionId);
      }).pipe(Effect.scoped, Effect.provide(Layer.merge(layerSessionStore(), TestClock.layer()))),
  );

  it.effect("expires a connected session at its credential deadline", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const client = yield* sessions.issue(clientSessionInput);
      yield* sessions.markConnected(client.sessionId);
      const invalidated = yield* Deferred.make<void>();
      const watcher = yield* sessions
        .awaitInvalidation(client.sessionId)
        .pipe(
          Effect.andThen(Deferred.succeed(invalidated, undefined)),
          Effect.forkScoped({ startImmediately: true }),
        );
      yield* TestClock.adjust(Duration.minutes(59));
      expect(yield* Deferred.isDone(invalidated)).toBe(false);
      yield* TestClock.adjust(Duration.minutes(1));
      yield* Fiber.join(watcher);
      yield* sessions.awaitInvalidation(client.sessionId);
    }).pipe(Effect.scoped, Effect.provide(Layer.merge(layerSessionStore(), TestClock.layer()))),
  );

  it.effect("carries a runtime-mode ceiling only on sessions issued with one", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const capped = yield* sessions.issue({
        subject: "mcp-client",
        runtimeModeCeiling: "auto",
      });
      const uncapped = yield* sessions.issue({ subject: "one-time-token" });

      expect((yield* sessions.verify(capped.token)).runtimeModeCeiling).toBe("auto");
      expect((yield* sessions.verify(uncapped.token)).runtimeModeCeiling).toBeUndefined();
    }).pipe(Effect.provide(layerSessionStore())),
  );
  it.effect("takes scopes from the session record", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const secrets = yield* ServerSecretStore.ServerSecretStore;
      const legacyScopes = ["orchestration:read", "terminal:operate", "review:write"] as const;
      const issued = yield* sessions.issue({ subject: "one-time-token", scopes: legacyScopes });
      const [encodedPayload] = issued.token.split(".");
      const currentClaims = base64UrlDecodeUtf8(encodedPayload!);
      expect(currentClaims).toContain('"v":1');
      const signer = yield* PayloadSigner.fromSecret(
        yield* secrets.getOrCreateRandom("server-signing-key", 32),
      );
      const resign = (claims: string) => {
        const payload = base64UrlEncode(claims);
        return signer.sign(payload).pipe(Effect.map((signature) => `${payload}.${signature}`));
      };
      const legacyToken = yield* resign(currentClaims.replace('"v":1', '"v":2'));
      const editedToken = yield* resign(
        currentClaims.replace(/"scopes":\[[^\]]*\]/, '"scopes":["access:write"]'),
      );

      expect((yield* sessions.verify(issued.token)).scopes).toEqual(legacyScopes);
      expect((yield* sessions.verify(legacyToken)).scopes).toEqual(legacyScopes);
      expect((yield* sessions.verify(editedToken)).scopes).toEqual(legacyScopes);
    }).pipe(
      Effect.provide(
        SessionStore.layer.pipe(
          Layer.provideMerge(ServerSecretStore.layer),
          Layer.provide(SqlitePersistence.layerMemory),
          Layer.provide(layerServerConfig),
        ),
      ),
    ),
  );

  it.effect("atomically replaces active sessions with the same subject", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const paired = yield* sessions.issue({ subject: "one-time-token" });
      const [firstBearer, secondBearer] = yield* Effect.all(
        [
          sessions.issue({
            subject: "desktop-bootstrap",
            replaceActiveForSubject: true,
          }),
          sessions.issue({
            subject: "desktop-bootstrap",
            replaceActiveForSubject: true,
          }),
        ],
        { concurrency: "unbounded" },
      );

      const active = yield* sessions.listActive();
      const bearerVerification = yield* Effect.all([
        sessions.verify(firstBearer.token).pipe(Effect.option),
        sessions.verify(secondBearer.token).pipe(Effect.option),
      ]);

      expect(active).toHaveLength(2);
      expect(active.find((entry) => entry.sessionId === paired.sessionId)).toBeDefined();
      expect(active.filter((entry) => entry.subject === "desktop-bootstrap")).toHaveLength(1);
      expect(bearerVerification.filter(Option.isSome)).toHaveLength(1);
    }).pipe(Effect.provide(layerSessionStore())),
  );

  it.effect("keeps the previous desktop session valid when replacement fails", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const sql = yield* SqlClient.SqlClient;
      const previous = yield* sessions.issue({ subject: "desktop-bootstrap" });
      yield* sql`
        CREATE TRIGGER reject_auth_session_insert BEFORE INSERT ON auth_sessions
        BEGIN
          SELECT RAISE(ABORT, 'simulated insert failure');
        END
      `;

      const error = yield* sessions
        .issue({
          subject: "desktop-bootstrap",
          replaceActiveForSubject: true,
        })
        .pipe(Effect.flip);

      expect(error._tag).toBe("SessionCredentialIssueError");
      expect((yield* sessions.verify(previous.token)).sessionId).toBe(previous.sessionId);
      expect((yield* sessions.listActive()).map((session) => session.sessionId)).toEqual([
        previous.sessionId,
      ]);
    }).pipe(Effect.provide(Layer.mergeAll(layerSessionStore(), SqlitePersistence.layerMemory))),
  );

  it.effect.each(["insert", "revoke"] as const)(
    "keeps existing sessions valid when replacement cannot %s",
    (operation) =>
      Effect.gen(function* () {
        const sessions = yield* SessionStore.SessionStore;
        const sql = yield* SqlClient.SqlClient;
        const previous = yield* sessions.issue({ subject: "one-time-token" });
        const unrelated = yield* sessions.issue({ subject: "one-time-token" });
        if (operation === "insert") {
          yield* sql`
            CREATE TRIGGER reject_auth_session_insert BEFORE INSERT ON auth_sessions
            BEGIN
              SELECT RAISE(ABORT, 'simulated insert failure');
            END
          `;
        } else {
          yield* sql`
            CREATE TRIGGER reject_auth_session_revocation BEFORE UPDATE OF revoked_at ON auth_sessions
            BEGIN
              SELECT RAISE(ABORT, 'simulated revocation failure');
            END
          `;
        }

        const error = yield* sessions
          .issue({
            subject: "one-time-token",
            scopes: ["orchestration:read"],
            replaceActiveForSubject: true,
          })
          .pipe(Effect.flip);

        expect(error._tag).toBe("SessionCredentialIssueError");
        expect((yield* sessions.verify(previous.token)).sessionId).toBe(previous.sessionId);
        expect((yield* sessions.verify(unrelated.token)).sessionId).toBe(unrelated.sessionId);
        expect((yield* sessions.listActive()).map((session) => session.sessionId).sort()).toEqual(
          [previous.sessionId, unrelated.sessionId].sort(),
        );
      }).pipe(Effect.provide(Layer.mergeAll(layerSessionStore(), SqlitePersistence.layerMemory))),
  );

  it.effect("rejects websocket tokens once the parent session has expired", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const issued = yield* sessions.issue({
        subject: "short-lived",
        ttl: Duration.seconds(1),
      });
      const websocket = yield* sessions.issueWebSocketToken(issued.sessionId);

      yield* TestClock.adjust(Duration.seconds(2));

      const error = yield* Effect.flip(sessions.verifyWebSocketToken(websocket.token));
      expect(error._tag).toBe("WebSocketSessionExpiredError");
      if (error._tag === "WebSocketSessionExpiredError") {
        expect(error.sessionId).toBe(issued.sessionId);
        expect(error.expiresAt.epochMilliseconds).toBe(issued.expiresAt.epochMilliseconds);
        expect(error.observedAt.epochMilliseconds).toBeGreaterThan(
          error.expiresAt.epochMilliseconds,
        );
      }
    }).pipe(Effect.provide(Layer.merge(layerSessionStore(), TestClock.layer()))),
  );

  it.effect("lists active sessions, tracks connectivity, and revokes other sessions", () =>
    Effect.gen(function* () {
      const sessions = yield* SessionStore.SessionStore;
      const administrative = yield* sessions.issue({
        subject: "desktop-bootstrap",
        scopes: ["orchestration:read", "access:write"],
        client: {
          label: "Desktop app",
          deviceType: "desktop",
          os: "macOS",
        },
      });
      const client = yield* sessions.issue({
        subject: "one-time-token",
        scopes: ["orchestration:read"],
        client: {
          label: "Julius iPhone",
          deviceType: "mobile",
          os: "iOS",
          ipAddress: "192.168.1.88",
        },
      });
      const clientWebSocket = yield* sessions.issueWebSocketToken(client.sessionId);

      yield* sessions.markConnected(client.sessionId);
      const beforeRevoke = yield* sessions.listActive();
      const revokedCount = yield* sessions.revokeAllExcept(administrative.sessionId);
      const afterRevoke = yield* sessions.listActive();
      const revokedClient = yield* Effect.flip(sessions.verify(client.token));
      const revokedClientWebSocket = yield* Effect.flip(
        sessions.verifyWebSocketToken(clientWebSocket.token),
      );

      expect(beforeRevoke).toHaveLength(2);
      expect(beforeRevoke.find((entry) => entry.sessionId === client.sessionId)?.connected).toBe(
        true,
      );
      expect(beforeRevoke.find((entry) => entry.sessionId === client.sessionId)?.client.label).toBe(
        "Julius iPhone",
      );
      expect(
        beforeRevoke.find((entry) => entry.sessionId === administrative.sessionId)?.client
          .deviceType,
      ).toBe("desktop");
      expect(revokedCount).toBe(1);
      expect(afterRevoke).toHaveLength(1);
      expect(afterRevoke[0]?.sessionId).toBe(administrative.sessionId);
      expect(revokedClient._tag).toBe("SessionTokenRevokedError");
      if (revokedClient._tag === "SessionTokenRevokedError") {
        expect(revokedClient.sessionId).toBe(client.sessionId);
        expect(revokedClient.revokedAt.epochMilliseconds).toBeGreaterThanOrEqual(0);
      }
      expect(revokedClientWebSocket._tag).toBe("WebSocketSessionRevokedError");
      if (revokedClientWebSocket._tag === "WebSocketSessionRevokedError") {
        expect(revokedClientWebSocket.sessionId).toBe(client.sessionId);
        expect(revokedClientWebSocket.revokedAt.epochMilliseconds).toBeGreaterThanOrEqual(0);
      }
    }).pipe(Effect.provide(layerSessionStore())),
  );
});
