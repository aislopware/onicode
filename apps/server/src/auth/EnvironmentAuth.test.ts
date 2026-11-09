import * as NodePlatformCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { AuthStandardClientScopes } from "@onicode/contracts";
import { expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";

import * as ServerConfig from "../config.ts";
import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as EnvironmentAuth from "./EnvironmentAuth.ts";

import * as ServerSecretStore from "./ServerSecretStore.ts";
import * as SessionStore from "./SessionStore.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";

const TEST_SERVER_PORT = 13_773;

const layerServerConfig = (overrides?: Partial<ServerConfig.ServerConfig["Service"]>) =>
  Layer.effect(
    ServerConfig.ServerConfig,
    Effect.gen(function* () {
      const config = yield* ServerConfig.ServerConfig;
      return {
        ...config,
        ...overrides,
        port: TEST_SERVER_PORT,
      } satisfies ServerConfig.ServerConfig["Service"];
    }),
  ).pipe(Layer.provide(ServerConfig.layerTest(process.cwd(), { prefix: "oni-auth-server-test-" })));

const layerEnvironmentAuth = (overrides?: Partial<ServerConfig.ServerConfig["Service"]>) =>
  EnvironmentAuth.layer.pipe(
    Layer.provide(ManagedPolicy.layerTest()),
    Layer.provideMerge(SqlitePersistence.layerMemory),
    Layer.provide(ServerSecretStore.layer),
    Layer.provide(ServerEnvironment.layerIdentity),
    Layer.provide(layerServerConfig(overrides)),
  );

const makeBearerRequest = (
  token: string,
): Parameters<EnvironmentAuth.EnvironmentAuth["Service"]["authenticateHttpRequest"]>[0] =>
  ({
    headers: {
      authorization: `Bearer ${token}`,
    },
  }) as unknown as Parameters<
    EnvironmentAuth.EnvironmentAuth["Service"]["authenticateHttpRequest"]
  >[0];

const requestMetadata = {
  deviceType: "desktop" as const,
  os: "macOS",
  ipAddress: "192.168.1.23",
};

it.layer(Layer.mergeAll(NodeServices.layer, NodePlatformCrypto.layer))(
  "EnvironmentAuth.layer",
  (it) => {
    it.effect("preserves pairing grants after rejecting scopes they do not grant", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const pairingCredential = yield* serverAuth.issuePairingCredential({
          scopes: ["orchestration:read", "orchestration:operate"],
        });

        const error = yield* serverAuth
          .exchangeBootstrapCredentialForAccessToken(
            pairingCredential.credential,
            ["access:write"],
            requestMetadata,
          )
          .pipe(Effect.flip);

        expect(error._tag).toBe("ServerAuthScopeNotGrantedError");
        expect((yield* serverAuth.listPairingLinks()).map((link) => link.id)).toContain(
          pairingCredential.id,
        );
        expect(yield* serverAuth.listSessions()).toEqual([]);

        const token = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          pairingCredential.credential,
          ["orchestration:read"],
          requestMetadata,
        );
        const session = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(token.access_token),
        );

        expect(token.scope).toBe("orchestration:read");
        expect(session.scopes).toEqual(["orchestration:read"]);
        expect((yield* serverAuth.listPairingLinks()).map((link) => link.id)).not.toContain(
          pairingCredential.id,
        );
        const reused = yield* serverAuth
          .exchangeBootstrapCredentialForAccessToken(
            pairingCredential.credential,
            ["orchestration:read"],
            requestMetadata,
          )
          .pipe(Effect.flip);
        expect(reused._tag).toBe("ServerAuthInvalidCredentialError");
      }).pipe(Effect.provide(layerEnvironmentAuth())),
    );

    it.effect.each([
      { label: "omits scope", requestedScopes: undefined },
      {
        label: "requests unsupported permissions alongside a granted one",
        requestedScopes: ["orchestration:read", "access:write"] as const,
      },
    ])("inherits a constrained pairing grant when token exchange $label", ({ requestedScopes }) =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const pairingCredential = yield* serverAuth.issuePairingCredential({
          scopes: ["orchestration:read"],
        });

        const token = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          pairingCredential.credential,
          requestedScopes,
          requestMetadata,
        );

        expect(token.scope).toBe("orchestration:read");
      }).pipe(Effect.provide(layerEnvironmentAuth())),
    );

    it.effect(
      "preserves a rejected MCP pairing code for exactly one concurrent lower-access retry",
      () =>
        Effect.gen(function* () {
          const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
          const pairing = yield* serverAuth.issuePairingCredential({
            scopes: ["orchestration:read"],
          });

          const rejected = yield* Effect.flip(
            serverAuth.consumeMcpApprovalCode(pairing.credential, "full-access"),
          );
          expect(rejected).toMatchObject({
            _tag: "ServerAuthMcpApprovalCodeError",
            reason: "insufficient_scope",
          });

          const retry = Effect.result(
            serverAuth.consumeMcpApprovalCode(pairing.credential, "read-only"),
          );
          const results = yield* Effect.all([retry, retry], { concurrency: "unbounded" });

          expect(results.filter((result) => result._tag === "Success")).toHaveLength(1);
          expect(results.filter((result) => result._tag === "Failure")).toMatchObject([
            { failure: { _tag: "ServerAuthMcpApprovalCodeError", reason: "unknown_or_used" } },
          ]);
        }).pipe(Effect.provide(layerEnvironmentAuth())),
    );

    it.effect("narrows seeded desktop grants to the requested scopes", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const token = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          "desktop-bootstrap-token",
          ["orchestration:read"],
          requestMetadata,
        );
        const session = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(token.access_token),
        );

        expect(token.scope).toBe("orchestration:read");
        expect(session.scopes).toEqual(["orchestration:read"]);
      }).pipe(
        Effect.provide(layerEnvironmentAuth({ desktopBootstrapToken: "desktop-bootstrap-token" })),
      ),
    );

    it.effect("rotates desktop bearer sessions without accumulating authorized clients", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const sessions = yield* SessionStore.SessionStore;
        const staleSessions = yield* Effect.forEach([1, 2, 3], () =>
          sessions.issue({ subject: "desktop-bootstrap" }),
        );
        const pairing = yield* serverAuth.issuePairingCredential();
        const paired = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          pairing.credential,
          undefined,
          { ...requestMetadata, label: "Onicode Desktop" },
        );
        const first = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          "desktop-bootstrap-token",
          undefined,
          requestMetadata,
        );
        const firstSession = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(first.access_token),
        );
        const second = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          "desktop-bootstrap-token",
          undefined,
          requestMetadata,
        );

        const active = yield* serverAuth.listSessions();
        const firstError = yield* serverAuth
          .authenticateHttpRequest(makeBearerRequest(first.access_token))
          .pipe(Effect.flip);
        const secondSession = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(second.access_token),
        );

        expect(active).toHaveLength(2);
        expect(active.map((entry) => entry.sessionId)).toContain(secondSession.sessionId);
        expect(active.map((entry) => entry.sessionId)).not.toContain(firstSession.sessionId);
        expect(firstError._tag).toBe("ServerAuthInvalidCredentialError");
        for (const stale of staleSessions) {
          const error = yield* sessions.verify(stale.token).pipe(Effect.flip);
          expect(error._tag).toBe("SessionTokenRevokedError");
        }
        const pairedSession = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(paired.access_token),
        );
        expect(pairedSession.subject).toBe("one-time-token");
        expect(active.map((entry) => entry.sessionId)).toContain(pairedSession.sessionId);
      }).pipe(
        Effect.provide(
          layerEnvironmentAuth({
            desktopBootstrapToken: "desktop-bootstrap-token",
          }),
        ),
      ),
    );

    it.effect("issues startup pairing credentials that bootstrap administrative sessions", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;

        const startup = yield* serverAuth.issueStartupPairingCredential();
        const listedPairingLinks = yield* serverAuth.listPairingLinks();
        expect(
          listedPairingLinks.some(
            (pairingLink) => pairingLink.subject === "administrative-bootstrap",
          ),
        ).toBe(false);

        const exchanged = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          startup.credential,
          undefined,
          requestMetadata,
        );
        const verified = yield* serverAuth.authenticateHttpRequest(
          makeBearerRequest(exchanged.access_token),
        );

        expect(verified.scopes).toEqual(AuthStandardClientScopes);
        expect(verified.subject).toBe("administrative-bootstrap");
      }).pipe(Effect.provide(layerEnvironmentAuth())),
    );

    it.effect(
      "lists pairing links and revokes other sessions while keeping the administrative session",
      () =>
        Effect.gen(function* () {
          const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
          const administrativeExchange =
            yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
              "desktop-bootstrap-token",
              undefined,
              requestMetadata,
            );
          const administrativeSession = yield* serverAuth.authenticateHttpRequest(
            makeBearerRequest(administrativeExchange.access_token),
          );
          const pairingCredential = yield* serverAuth.issuePairingCredential({
            label: "Julius iPhone",
          });
          const listedPairingLinks = yield* serverAuth.listPairingLinks();
          const clientExchange = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
            pairingCredential.credential,
            undefined,
            {
              ...requestMetadata,
              deviceType: "mobile",
              os: "iOS",
              ipAddress: "192.168.1.88",
            },
          );
          const clientSession = yield* serverAuth.authenticateHttpRequest(
            makeBearerRequest(clientExchange.access_token),
          );
          const clientsBeforeRevoke = yield* serverAuth.listClientSessions(
            administrativeSession.sessionId,
          );
          const revokedCount = yield* serverAuth.revokeOtherClientSessions(
            administrativeSession.sessionId,
          );
          const clientsAfterRevoke = yield* serverAuth.listClientSessions(
            administrativeSession.sessionId,
          );

          expect(listedPairingLinks.map((entry) => entry.id)).toContain(pairingCredential.id);
          expect(listedPairingLinks.find((entry) => entry.id === pairingCredential.id)?.label).toBe(
            "Julius iPhone",
          );
          expect(clientsBeforeRevoke).toHaveLength(2);
          expect(
            clientsBeforeRevoke.find((entry) => entry.sessionId === administrativeSession.sessionId)
              ?.current,
          ).toBe(true);
          expect(
            clientsBeforeRevoke.find((entry) => entry.sessionId === clientSession.sessionId)
              ?.current,
          ).toBe(false);
          expect(
            clientsBeforeRevoke.find((entry) => entry.sessionId === clientSession.sessionId)?.client
              .label,
          ).toBe("Julius iPhone");
          expect(
            clientsBeforeRevoke.find((entry) => entry.sessionId === clientSession.sessionId)?.client
              .deviceType,
          ).toBe("mobile");
          expect(revokedCount).toBe(1);
          expect(clientsAfterRevoke).toHaveLength(1);
          expect(clientsAfterRevoke[0]?.sessionId).toBe(administrativeSession.sessionId);
        }).pipe(
          Effect.provide(
            layerEnvironmentAuth({
              desktopBootstrapToken: "desktop-bootstrap-token",
            }),
          ),
        ),
    );

    it.effect("admits only peer links and the thread CLI as MCP clients", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const sessions = yield* SessionStore.SessionStore;
        const authenticate = (token: string) =>
          serverAuth.authenticateMcpClient(makeBearerRequest(token));
        const issue = (purpose: "peer-link" | "thread-cli") =>
          serverAuth.issueMcpClientSession({
            label: purpose,
            access: "auto",
            purpose,
            client: { deviceType: "bot" },
          });

        expect(yield* authenticate((yield* issue("peer-link")).token)).toMatchObject({
          access: "auto",
          peerLink: true,
        });
        expect(yield* authenticate((yield* issue("thread-cli")).token)).toMatchObject({
          access: "auto",
          peerLink: false,
        });
        // Outside agents signed in through OAuth got an mcp-client session without the claim.
        const outsideAgent = yield* sessions.issue({
          subject: "mcp-client",
          scopes: ["orchestration:read", "orchestration:operate"],
          runtimeModeCeiling: "auto",
        });
        const rejected = yield* Effect.flip(authenticate(outsideAgent.token));
        expect(rejected._tag).toBe("ServerAuthInvalidCredentialError");
        const threadCli = yield* issue("thread-cli");
        const outsideMcp = yield* Effect.flip(
          serverAuth.authenticateHttpRequest(makeBearerRequest(threadCli.token)),
        );
        expect(outsideMcp._tag).toBe("ServerAuthInvalidCredentialError");
      }).pipe(Effect.provide(layerEnvironmentAuth())),
    );

    it.effect.each(["Bearer", "bearer", "bEaReR"])(
      "accepts %s for HTTP and socket upgrades regardless of page origin",
      (scheme) =>
        Effect.gen(function* () {
          const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
          const bearer = yield* serverAuth.issueSession();
          const request = {
            url: "/ws",
            headers: {
              host: "192.168.1.50:3773",
              origin: "http://192.168.1.50:8080",
              authorization: `${scheme} ${bearer.token}`,
            },
          } as never;
          const verified = yield* serverAuth.authenticateWebSocketUpgrade(request, "rpc");
          expect(verified.sessionId).toBe(bearer.sessionId);
          expect((yield* serverAuth.authenticateHttpRequest(request)).sessionId).toBe(
            bearer.sessionId,
          );
        }).pipe(Effect.provide(layerEnvironmentAuth({ mode: "standalone", host: "192.168.1.50" }))),
    );

    it.effect("binds a socket ticket to the route it was issued for", () =>
      Effect.gen(function* () {
        const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
        const sessions = yield* SessionStore.SessionStore;
        const exchanged = yield* serverAuth.exchangeBootstrapCredentialForAccessToken(
          "desktop-bootstrap-token",
          ["orchestration:read"],
          requestMetadata,
        );
        const session = yield* sessions.verify(exchanged.access_token);
        const upgrade = (ticket: string, use: "rpc" | "media") =>
          serverAuth.authenticateWebSocketUpgrade(
            {
              method: "GET",
              url: `/ws?orchestrationProtocol=2&wsTicket=${ticket}`,
              headers: { host: "192.168.1.50:3773" },
            } as never,
            use,
          );

        const rpc = yield* serverAuth.issueWebSocketTicket(session, { use: "rpc" });
        expect((yield* upgrade(Redacted.value(rpc.ticket), "rpc")).sessionId).toBe(
          session.sessionId,
        );
        const rpcOnMedia = yield* Effect.flip(upgrade(Redacted.value(rpc.ticket), "media"));
        expect(rpcOnMedia._tag).toBe("ServerAuthInvalidCredentialError");

        const media = yield* serverAuth.issueWebSocketTicket(session, { use: "media" });
        expect((yield* upgrade(Redacted.value(media.ticket), "media")).sessionId).toBe(
          session.sessionId,
        );
        const mediaOnRpc = yield* Effect.flip(upgrade(Redacted.value(media.ticket), "rpc"));
        expect(mediaOnRpc._tag).toBe("ServerAuthInvalidCredentialError");

        // Clients that predate ticket binding still connect with the unbound ticket they mint.
        const legacy = yield* serverAuth.issueWebSocketTicket(session);
        expect((yield* upgrade(Redacted.value(legacy.ticket), "rpc")).sessionId).toBe(
          session.sessionId,
        );
      }).pipe(
        Effect.provide(layerEnvironmentAuth({ desktopBootstrapToken: "desktop-bootstrap-token" })),
      ),
    );
  },
);
