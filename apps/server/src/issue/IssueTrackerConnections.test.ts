import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as SqlClient from "effect/sql/SqlClient";
import { IssueTrackerKind, type ProjectId } from "@onicode/contracts";
import {
  IssueTrackerError,
  type IssueTrackerCredential,
  type StandaloneIssueTrackerApi,
} from "@onicode/source-control-core/server/IssueTracker";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as IssueTrackerBindings from "../persistence/IssueTrackerBindings.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as IssueTrackerConnections from "./IssueTrackerConnections.ts";
import * as IssueTrackerRegistry from "./IssueTrackerRegistry.ts";

const LINEAR = IssueTrackerKind.make("linear");
const JIRA = IssueTrackerKind.make("jira");

function connectionsLayer(
  tracker: StandaloneIssueTrackerApi,
  secrets: Map<string, string>,
  written: Array<string> = [],
) {
  let random = 0;
  return IssueTrackerConnections.layer.pipe(
    Layer.provideMerge(IssueTrackerBindings.layer),
    Layer.provideMerge(
      Layer.mergeAll(
        SqlitePersistence.layerMemory,
        Layer.succeed(
          IssueTrackerRegistry.IssueTrackerRegistry,
          IssueTrackerRegistry.fromTrackers([tracker]),
        ),
        Layer.mock(ServerSecretStore.ServerSecretStore)({
          get: (name) =>
            Effect.succeed(
              Option.map(Option.fromNullishOr(secrets.get(name)), (value) =>
                new TextEncoder().encode(value),
              ),
            ),
          set: (name, value) =>
            Effect.sync(() => {
              written.push(name);
              secrets.set(name, new TextDecoder().decode(value));
            }),
          remove: (name) => Effect.sync(() => void secrets.delete(name)),
        }),
        Layer.succeed(
          SecureRandom.SecureRandom,
          SecureRandom.make((size) => new Uint8Array(size).fill(++random)),
        ),
      ),
    ),
  );
}

describe("IssueTrackerConnections", () => {
  it.effect("keeps each account's key and bindings apart, and no key past its account", () => {
    const secrets = new Map<string, string>();
    const written: Array<string> = [];
    const probing = Deferred.makeUnsafe<void>();
    const released = Deferred.makeUnsafe<void>();
    const tracker = {
      kind: LINEAR,
      standalone: {
        tracker: LINEAR,
        host: "linear.app",
        credential: { label: "API key", helpUrl: null },
      },
      probeCredential: ({ token }: { readonly token: string }) =>
        token === "bad"
          ? Effect.fail(
              new IssueTrackerError({
                tracker: LINEAR,
                operation: "viewer",
                reason: "unauthenticated",
                detail: "Linear refused the key.",
              }),
            )
          : Effect.as(
              token === "slow"
                ? Effect.andThen(Deferred.succeed(probing, undefined), Deferred.await(released))
                : Effect.void,
              { account: `user-${token}`, scopes: [] },
            ),
    } as unknown as StandaloneIssueTrackerApi;
    const layer = connectionsLayer(tracker, secrets, written);
    return Effect.gen(function* () {
      const connections = yield* IssueTrackerConnections.IssueTrackerConnections;
      yield* connections.connect({ tracker: LINEAR, token: "work" });
      const { accountId } = yield* connections.connect({
        tracker: LINEAR,
        token: "home",
        addAccount: true,
      });

      // The first account keeps the name it was saved under before accounts existed.
      expect(Object.fromEntries(secrets)).toEqual({
        "issue-tracker-bGluZWFy-token": "work",
        [`issue-tracker-bGluZWFy-account-${accountId}-token`]: "home",
      });
      yield* connections.bindProject({
        projectId: "work" as ProjectId,
        binding: { tracker: LINEAR, scope: "ENG" },
      });
      yield* connections.bindProject({
        projectId: "home" as ProjectId,
        binding: { tracker: LINEAR, accountId, scope: "ENG" },
      });
      // Older clients read `bindings` as the first account's, so the second one's are apart.
      expect(yield* connections.list).toMatchObject({
        accounts: [{ accountId, account: "user-home" }],
        bindings: { work: { tracker: LINEAR, scope: "ENG" } },
        accountBindings: { home: { tracker: LINEAR, accountId, scope: "ENG" } },
      });
      // An older client offers the added account's project as unbound; its unbind leaves it.
      yield* connections.bindProject({ projectId: "home" as ProjectId, binding: null });
      expect((yield* connections.list).accountBindings).toHaveProperty("home");
      // Nor does its bind replace it with the first account's; a bind that names it does.
      expect(
        yield* Effect.flip(
          connections.bindProject({
            projectId: "home" as ProjectId,
            binding: { tracker: LINEAR, scope: "ENG" },
          }),
        ),
      ).toMatchObject({
        detail: "This project is bound with another account. Update Onicode to change it.",
      });
      expect((yield* connections.list).accountBindings).toMatchObject({ home: { accountId } });
      yield* connections.bindProject({
        projectId: "moved" as ProjectId,
        binding: { tracker: LINEAR, accountId, scope: "ENG" },
      });
      yield* connections.bindProject({
        projectId: "moved" as ProjectId,
        binding: { tracker: LINEAR, scope: "ENG" },
        ...(accountId === undefined ? {} : { accountId }),
      });
      expect((yield* connections.list).bindings).toHaveProperty("moved");
      yield* Effect.flip(connections.connect({ tracker: LINEAR, token: "bad", addAccount: true }));
      expect(
        yield* Effect.flip(
          connections.connect({ tracker: LINEAR, token: "home", addAccount: true }),
        ),
      ).toMatchObject({ detail: "This credential is already connected as another account." });
      expect(secrets.size).toBe(2);

      // A removal does not wait on a replacement key's probe, and the replacement then finds its
      // account gone instead of storing a key for it.
      const replacing = yield* connections
        .connect({ tracker: LINEAR, accountId, token: "slow" })
        .pipe(Effect.forkChild({ startImmediately: true }));
      yield* Deferred.await(probing);
      yield* connections.connect({ tracker: LINEAR, accountId, token: null });
      yield* Deferred.succeed(released, undefined);
      expect(yield* Effect.flip(Fiber.join(replacing))).toMatchObject({
        detail: "This account is not saved on the server.",
      });
      expect([...secrets.keys()]).toEqual(["issue-tracker-bGluZWFy-token"]);
      const after = yield* connections.list;
      expect([after.accounts, after.accountBindings]).toEqual([[], {}]);
      expect(
        yield* Effect.flip(
          connections.bindProject({
            projectId: "home" as ProjectId,
            binding: { tracker: LINEAR, accountId, scope: "ENG" },
          }),
        ),
      ).toMatchObject({ detail: "This account is not saved on the server." });

      // An account that cannot be saved takes its new key with it.
      yield* (yield* SqlClient.SqlClient)`
        CREATE TRIGGER refuse_account BEFORE INSERT ON issue_tracker_accounts
        BEGIN SELECT RAISE(ABORT, 'refused'); END
      `;
      written.length = 0;
      expect(
        yield* Effect.flip(
          connections.connect({ tracker: LINEAR, token: "home", addAccount: true }),
        ),
      ).toMatchObject({ detail: "The account could not be saved." });
      expect(written).toHaveLength(1);
      expect([...secrets.keys()]).toEqual(["issue-tracker-bGluZWFy-token"]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("reuses a saved account's fields when an old app replaces its key alone", () => {
    const secrets = new Map<string, string>();
    const probed: Array<IssueTrackerCredential> = [];
    let workKey = "";
    const tracker = {
      kind: JIRA,
      standalone: {
        tracker: JIRA,
        displayName: "Jira",
        host: "atlassian.net",
        credential: {
          label: "API token",
          helpUrl: null,
          fields: [
            { key: "site", label: "Site" },
            { key: "email", label: "Account email" },
          ],
        },
      },
      probeCredential: (credential: IssueTrackerCredential) =>
        Effect.sync(() => {
          probed.push(credential);
          // Another client disconnects the account while this probe is in flight.
          if (credential.token === "racing") secrets.delete(workKey);
          return { account: credential.fields["email"], scopes: [] };
        }),
    } as unknown as StandaloneIssueTrackerApi;
    return Effect.gen(function* () {
      const connections = yield* IssueTrackerConnections.IssueTrackerConnections;
      const work = { site: "acme.atlassian.net", email: "j@acme.test" };
      const home = { site: "home.atlassian.net", email: "j@home.test" };
      yield* connections.connect({ tracker: JIRA, token: "work", fields: work });
      workKey = [...secrets.keys()][0]!;
      const { accountId } = yield* connections.connect({
        tracker: JIRA,
        token: "home",
        addAccount: true,
        fields: home,
      });
      // An app from before the tracker had fields replaces a token with the token alone.
      yield* connections.connect({ tracker: JIRA, token: "work-new" });
      yield* connections.connect({ tracker: JIRA, accountId, token: "home-new" });
      probed.length = 0;
      yield* connections.list;
      expect(probed).toHaveLength(2);
      expect(probed).toEqual(
        expect.arrayContaining([
          { token: "work-new", fields: work },
          { token: "home-new", fields: home },
        ]),
      );

      expect(
        yield* Effect.flip(connections.connect({ tracker: JIRA, token: "new", addAccount: true })),
      ).toMatchObject({
        reason: "rejected",
        detail:
          "Jira also needs its site and account email, which this version of the app can't send. " +
          "Update the app, or connect Jira from the desktop app.",
      });

      // The fields read before the probe must not bring back an account disconnected meanwhile.
      expect(
        yield* Effect.flip(connections.connect({ tracker: JIRA, token: "racing" })),
      ).toMatchObject({ detail: "This Jira account changed while connecting. Try again." });
      expect(secrets.has(workKey)).toBe(false);
      expect(
        yield* Effect.flip(connections.connect({ tracker: JIRA, token: "work-again" })),
      ).toMatchObject({ reason: "rejected" });
    }).pipe(Effect.provide(connectionsLayer(tracker, secrets)));
  });
});
