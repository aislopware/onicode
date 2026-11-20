import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import {
  OrchestratorMcpRequestSecretInput,
  ProjectId,
  type OrchestrationV2ServerCommand,
  SecretProviderId,
  SecretProviderKind,
  ThreadId,
  TurnItemId,
} from "@onicode/contracts";
import { defineSecretProviderDriver } from "@onicode/secret-provider-core/server/driver";
import * as SecretResolver from "@onicode/secret-provider-core/server/SecretResolver";
import { defineSecretProvider } from "@onicode/secret-provider-core/shared/definition";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Tracer from "effect/Tracer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import * as TestClock from "effect/testing/TestClock";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as ThreadManagementService from "../orchestration-v2/ThreadManagementService.ts";
import * as SecretRequests from "./SecretRequests.ts";

const threadId = ThreadId.make("thread-orchestrator");
const turnItemId = TurnItemId.make("turn-item:secret-request:1");
const projectId = ProjectId.make("project-1");
const decodeSecretInput = Schema.decodeUnknownEffect(OrchestratorMcpRequestSecretInput);
const decodeSecretInputExit = Schema.decodeUnknownExit(OrchestratorMcpRequestSecretInput);

const FAKE_KIND = SecretProviderKind.make("fake");
const vault = SecretProviderId.make("vault");
/** Reads `fake://` references from `values` while unlocked, as a password manager would. */
const fakeVault = (gate: Effect.Effect<void> = Effect.void) => {
  const state = { unlocked: true, values: new Map([["fake://github/token", "ghp_from_vault"]]) };
  const driver = defineSecretProviderDriver({
    kind: FAKE_KIND,
    definition: defineSecretProvider({
      kind: FAKE_KIND,
      label: "Fake",
      configSchema: Schema.Struct({}),
      isReference: (reference) => reference.startsWith("fake://"),
      referencePlaceholder: "fake://item",
      invalidReferenceMessage: "Use fake://item.",
    }),
    read: (_config, references) =>
      gate.pipe(
        Effect.map(() =>
          references.map((reference) => (state.unlocked ? state.values.get(reference) : undefined)),
        ),
      ),
  });
  return {
    state,
    layer: SecretResolver.layer({
      drivers: [driver],
      secretProviders: Effect.succeed({ [vault]: { kind: FAKE_KIND, label: "Vault", config: {} } }),
    }).pipe(Layer.provide(Layer.merge(NodeServices.layer, NodeCrypto.layer))),
  };
};

const withService = <A, E>(
  body: (input: {
    readonly service: SecretRequests.SecretRequests["Service"];
    readonly stored: Map<string, Uint8Array>;
    readonly dispatched: Array<OrchestrationV2ServerCommand>;
    readonly moveThread: (to: ProjectId) => void;
    /** The next removal from the store fails, as a read-only disk would. */
    readonly failNextRemove: () => void;
  }) => Effect.Effect<A, E>,
  options: {
    readonly threadId?: ThreadId;
    readonly fields?: ReadonlyArray<{ readonly name: string; readonly label: string }>;
    readonly directory?: string;
    readonly runStatus?: string;
    readonly removeFails?: boolean;
    readonly closedFirst?: boolean;
    readonly failedRecords?: number;
    /** The first read of the thread after the card is saved fails. */
    readonly failedReadAfterSave?: boolean;
    readonly resolver?: Layer.Layer<SecretResolver.SecretResolver>;
    readonly afterStore?: Effect.Effect<void>;
  } = {},
) =>
  Effect.gen(function* () {
    const stored = new Map<string, Uint8Array>();
    const dispatched: Array<OrchestrationV2ServerCommand> = [];
    let secretStatus = "pending";
    let threadProjectId = projectId;
    let failedRecords = options.failedRecords ?? 0;
    let failNextRemove = false;
    let failReadAfterSave = options.failedReadAfterSave ?? false;
    const requestThreadId = options.threadId ?? threadId;
    const layerDependencies = Layer.mergeAll(
      NodeCrypto.layer,
      NodeServices.layer,
      options.resolver ?? fakeVault().layer,
      Layer.succeed(
        ServerSecretStore.ServerSecretStore,
        ServerSecretStore.ServerSecretStore.of({
          ...(options.directory === undefined ? {} : { directory: options.directory }),
          get: (name) => Effect.yieldNow.pipe(Effect.as(Option.fromNullishOr(stored.get(name)))),
          set: (name, value) =>
            Effect.sync(() => void stored.set(name, value)).pipe(
              Effect.andThen(options.afterStore ?? Effect.void),
            ),
          create: (name, value) =>
            stored.has(name)
              ? Effect.fail(
                  new ServerSecretStore.SecretStorePersistError({
                    resource: name,
                    cause: new PlatformError.PlatformError(
                      new PlatformError.SystemError({
                        _tag: "AlreadyExists",
                        module: "FileSystem",
                        method: "open",
                      }),
                    ),
                  } as never),
                )
              : Effect.sync(() => void stored.set(name, value)),
          getOrCreateRandom: (name, bytes) =>
            Effect.sync(() => {
              const existing = stored.get(name);
              if (existing) return existing;
              const value = new Uint8Array(bytes).fill(7);
              stored.set(name, value);
              return value;
            }),
          remove: (name) =>
            options.removeFails || failNextRemove
              ? Effect.sync(() => {
                  failNextRemove = false;
                }).pipe(
                  Effect.andThen(
                    Effect.fail(
                      new ServerSecretStore.SecretStorePersistError({
                        resource: name,
                        cause: new Error("read-only"),
                      }),
                    ),
                  ),
                )
              : Effect.sync(() => void stored.delete(name)),
        }),
      ),
      Layer.mock(ThreadManagementService.ThreadManagementService)({
        // Reads yield like a real store, so two answers can both see a pending card.
        getThreadRecords: () =>
          Effect.yieldNow.pipe(
            Effect.andThen(() => {
              if (!failReadAfterSave || secretStatus !== "saved") return Effect.void;
              failReadAfterSave = false;
              return Effect.fail(new Error("database busy") as never);
            }),
            Effect.map(
              () =>
                ({
                  thread: { projectId: threadProjectId },
                  runs: [{ id: "run-1", status: options.runStatus ?? "running" }],
                  turnItems: [
                    {
                      id: turnItemId,
                      threadId: requestThreadId,
                      runId: "run-1",
                      nodeId: "node-root",
                      type: "secret_request",
                      label: "GitHub token",
                      reason: "Used as GH_TOKEN.",
                      url: "https://github.com/settings/tokens",
                      ...(options.fields === undefined ? {} : { fields: options.fields }),
                      secretStatus,
                    },
                  ],
                }) as never,
            ),
          ),
        dispatch: (command) => {
          if (command.type === "secret_request.record" && failedRecords > 0) {
            failedRecords -= 1;
            return Effect.fail(new Error("orchestrator unavailable") as never);
          }
          return Effect.sync(() => {
            dispatched.push(command);
            if (options.closedFirst && command.type === "secret_request.record") {
              secretStatus = "cancelled";
            } else if (command.type === "secret_request.record" && secretStatus === "pending") {
              secretStatus = command.secretStatus;
            }
            return {} as never;
          });
        },
      }),
    );
    return yield* Effect.gen(function* () {
      const service = yield* SecretRequests.SecretRequests;
      return yield* body({
        service,
        stored,
        dispatched,
        moveThread: (to) => {
          threadProjectId = to;
        },
        failNextRemove: () => {
          failNextRemove = true;
        },
      });
    }).pipe(Effect.provide(SecretRequests.layer.pipe(Layer.provide(layerDependencies))));
  });

/** `withService` with its secret files in a fresh temporary directory. */
const withFiles = <A, E>(
  body: (
    input: Parameters<Parameters<typeof withService>[0]>[0] & {
      readonly fs: FileSystem.FileSystem;
      readonly directory: string;
    },
  ) => Effect.Effect<A, E>,
  options: Omit<NonNullable<Parameters<typeof withService>[1]>, "directory"> = {},
) =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    return yield* withService((input) => body({ ...input, fs, directory }), {
      ...options,
      directory,
    });
  }).pipe(Effect.provide(NodeServices.layer));

const singleFile = (saved: { readonly file: string } | { readonly files: unknown }) =>
  "file" in saved ? saved.file : assert.fail("expected a single file");

const valuesOf = (stored: Map<string, Uint8Array>) =>
  Array.from(stored.entries())
    .filter(([name]) => name !== "secret-request-salt")
    .map(([, bytes]) => new TextDecoder().decode(bytes));

it.effect("a saved answer reaches the agent once, and the thread only learns it was saved", () =>
  withFiles(({ service, stored, dispatched, fs }) =>
    Effect.gen(function* () {
      yield* service.answer({
        threadId,
        turnItemId,
        answer: { type: "save", secret: "ghp_secret" },
      });
      assert.equal(dispatched.length, 1);
      assert.include(dispatched[0] as object, {
        type: "secret_request.record",
        secretStatus: "saved",
        url: "https://github.com/settings/tokens",
      });
      assert.notInclude(JSON.stringify(dispatched), "ghp_secret");

      const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
      assert.equal(yield* fs.readFileString(file), "ghp_secret");
      assert.isFalse(valuesOf(stored).some((value) => value.includes("ghp_secret")));
      yield* fs.remove(file);
      const again = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
      assert.equal(again.reason, "ref_unavailable");
      assert.include(again.message, "already delivered");
    }),
  ),
);

it.effect("a secret provider reference is read once and saved like a typed value", () =>
  withFiles(({ service, dispatched, fs }) =>
    Effect.gen(function* () {
      yield* service.answer({
        threadId,
        turnItemId,
        answer: {
          type: "reference",
          reference: { provider: vault, reference: "fake://github/token" },
        },
      });
      assert.include(dispatched[0] as object, { secretStatus: "saved" });
      assert.notInclude(JSON.stringify(dispatched), "fake://");
      const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
      assert.equal(yield* fs.readFileString(file), "ghp_from_vault");
    }),
  ),
);

it.effect(
  "an unreadable reference leaves the request pending, and a retry after unlocking works",
  () => {
    const { state, layer } = fakeVault();
    state.unlocked = false;
    return withFiles(
      ({ service, stored, dispatched, fs }) =>
        Effect.gen(function* () {
          const reference = { provider: vault, reference: "fake://github/token" };
          const locked = yield* service
            .answer({ threadId, turnItemId, answer: { type: "reference", reference } })
            .pipe(Effect.flip);
          assert.equal(locked.reason, "reference_unreadable");
          assert.include(locked.message, '"Vault"');
          assert.deepEqual(dispatched, []);
          assert.deepEqual(valuesOf(stored), []);

          state.unlocked = true;
          yield* service.answer({ threadId, turnItemId, answer: { type: "reference", reference } });
          const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
          assert.equal(yield* fs.readFileString(file), "ghp_from_vault");
        }),
      { resolver: layer },
    );
  },
);

it.effect("a reference to a secret provider that is not configured fails", () =>
  withService(({ service, dispatched }) =>
    Effect.gen(function* () {
      const error = yield* service
        .answer({
          threadId,
          turnItemId,
          answer: {
            type: "reference",
            reference: {
              // Also an Object.prototype key, which must not pass for a configured provider.
              provider: SecretProviderId.make("constructor"),
              reference: "fake://github/token",
            },
          },
        })
        .pipe(Effect.flip);
      assert.equal(error.reason, "reference_unreadable");
      assert.include(error.message, '"constructor"');
      assert.deepEqual(dispatched, []);
    }),
  ),
);

it.effect("two concurrent deliveries of one answer both get the one file it was written to", () =>
  withFiles(({ service, stored, fs }) =>
    Effect.gen(function* () {
      yield* service.answer({
        threadId,
        turnItemId,
        answer: { type: "save", secret: "ghp_secret" },
      });
      const [first, second] = yield* Effect.all(
        [
          service.saveToFile({ threadId, turnItemId }),
          service.saveToFile({ threadId, turnItemId }),
        ],
        { concurrency: "unbounded" },
      );
      assert.deepEqual(first, second);
      assert.equal(yield* fs.readFileString(singleFile(first)), "ghp_secret");
      assert.deepEqual(valuesOf(stored), []);
    }),
  ),
);

it.effect("a save that loses to the card closing deletes the value and says so", () =>
  withService(
    ({ service, stored }) =>
      Effect.gen(function* () {
        const error = yield* service
          .answer({ threadId, turnItemId, answer: { type: "save", secret: "ghp_secret" } })
          .pipe(Effect.flip);
        assert.equal(error.reason, "agent_stopped");
        assert.isFalse(valuesOf(stored).some((value) => value.includes("ghp_secret")));
      }),
    { closedFirst: true },
  ),
);

it.effect("a recorded save stands when reading the card back fails", () =>
  withFiles(
    ({ service, fs }) =>
      Effect.gen(function* () {
        yield* service.answer({
          threadId,
          turnItemId,
          answer: { type: "save", secret: "ghp_secret" },
        });
        assert.equal(
          yield* fs.readFileString(singleFile(yield* service.saveToFile({ threadId, turnItemId }))),
          "ghp_secret",
        );
      }),
    { failedReadAfterSave: true },
  ),
);

it.effect("a save whose record failed can be saved again", () =>
  withFiles(
    ({ service, fs }) =>
      Effect.gen(function* () {
        const failed = yield* service
          .answer({ threadId, turnItemId, answer: { type: "save", secret: "ghp_secret" } })
          .pipe(Effect.flip);
        assert.equal(failed.reason, "record_failed");
        yield* service.answer({
          threadId,
          turnItemId,
          answer: { type: "save", secret: "ghp_secret" },
        });
        const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
        assert.equal(yield* fs.readFileString(file), "ghp_secret");
      }),
    { failedRecords: 1 },
  ),
);

it.effect("a saved answer is only delivered in the project it was entered for", () =>
  withFiles(({ service, moveThread, fs }) =>
    Effect.gen(function* () {
      yield* service.answer({
        threadId,
        turnItemId,
        answer: { type: "save", secret: "ghp_secret" },
      });
      moveThread(ProjectId.make("project-other"));
      const elsewhere = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
      assert.include(elsewhere.message, "does not exist");
      moveThread(projectId);
      const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
      assert.equal(yield* fs.readFileString(file), "ghp_secret");
    }),
  ),
);

it.effect("declining stores nothing, and a request is answered once", () =>
  withService(({ service, stored, dispatched }) =>
    Effect.gen(function* () {
      yield* service.answer({ threadId, turnItemId, answer: { type: "decline" } });
      assert.deepEqual(valuesOf(stored), []);
      const refused = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
      assert.equal(refused.reason, "not_found");
      const late = yield* service
        .answer({ threadId, turnItemId, answer: { type: "save", secret: "ghp_secret" } })
        .pipe(Effect.flip);
      assert.include(late.message, "already answered");
      assert.equal(dispatched.length, 1);
      assert.deepEqual(valuesOf(stored), []);
    }),
  ),
);

it.effect("traces a saved answer and its delivery without ever recording the value", () =>
  Effect.gen(function* () {
    const spans: Array<Tracer.NativeSpan> = [];
    const tracer = Tracer.make({
      span: (options) => {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    });
    yield* withFiles(({ service }) =>
      Effect.gen(function* () {
        yield* service.answer({
          threadId,
          turnItemId,
          answer: { type: "save", secret: "ghp_secret" },
        });
        yield* service.saveToFile({ threadId, turnItemId });
      }),
    ).pipe(Effect.withTracer(tracer));
    const recorded = spans.flatMap((span) => [
      span.name,
      ...Array.from(span.attributes.values(), String),
    ]);
    assert.include(recorded, "SecretRequests.answer");
    assert.include(recorded, "SecretRequests.saveToFile");
    assert.isFalse(recorded.some((value) => value.includes("ghp_secret")));
  }),
);

it.effect("works in threads with long ids, such as a delegated subagent's", () => {
  const delegatedThreadId = ThreadId.make(
    `thread:delegated-task:command%3Amcp%3A${"a".repeat(36)}%3Adelegate-task%3Arelease-notes-v0.2.0-${"b".repeat(40)}`,
  );
  return withFiles(
    ({ service, stored, fs }) =>
      Effect.gen(function* () {
        yield* service.answer({
          threadId: delegatedThreadId,
          turnItemId,
          answer: { type: "save", secret: "ghp_secret" },
        });
        assert.isTrue(Array.from(stored.keys()).every((name) => name.length < 100));
        const file = singleFile(
          yield* service.saveToFile({ threadId: delegatedThreadId, turnItemId }),
        );
        assert.equal(yield* fs.readFileString(file), "ghp_secret");
      }),
    { threadId: delegatedThreadId },
  );
});

it.effect("refuses an answer once the agent that asked has stopped", () =>
  withService(
    ({ service, stored, dispatched }) =>
      Effect.gen(function* () {
        const late = yield* service
          .answer({ threadId, turnItemId, answer: { type: "save", secret: "ghp_secret" } })
          .pipe(Effect.flip);
        assert.include(late.message, "has stopped");
        assert.deepEqual(valuesOf(stored), []);
        assert.equal(dispatched.length, 0);
      }),
    { runStatus: "completed" },
  ),
);

const SECRET_FILE_TTL_MS = 24 * 60 * 60 * 1000;

it.effect("a file request moves its value to an owner-only file, once", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    const file = yield* withService(
      ({ service, stored, dispatched }) =>
        Effect.gen(function* () {
          yield* service.answer({
            threadId,
            turnItemId,
            answer: { type: "save", secret: "cf_token'$(id)" },
          });
          assert.include(dispatched[0] as object, { secretStatus: "saved" });
          const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
          assert.equal(yield* fs.readFileString(file), "cf_token'$(id)");
          assert.deepEqual(valuesOf(stored), []);
          if ((yield* HostProcess.Platform) !== "win32") {
            assert.equal((yield* fs.stat(file)).mode & 0o777, 0o600);
            assert.equal((yield* fs.stat(path.dirname(file))).mode & 0o777, 0o700);
          }
          // A retried request_secret gets the same file.
          assert.deepEqual(yield* service.saveToFile({ threadId, turnItemId }), { file });
          return file;
        }),
      { directory },
    );

    const { mtime } = yield* fs.stat(file);
    yield* TestClock.setTime(Option.getOrThrow(mtime).getTime() + SECRET_FILE_TTL_MS);
    yield* withService(() => Effect.void, { directory });
    assert.isTrue(yield* fs.exists(file));
    yield* TestClock.setTime(Option.getOrThrow(mtime).getTime() + SECRET_FILE_TTL_MS + 1);
    yield* withService(() => Effect.void, { directory });
    assert.isFalse(yield* fs.exists(file));
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a fields request keeps each value apart and delivers one file per field", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    yield* withService(
      ({ service, stored, dispatched, failNextRemove }) =>
        Effect.gen(function* () {
          const answer = (values: Record<string, string>) =>
            service.answer({ threadId, turnItemId, answer: { type: "save", values } });
          // An older client answers with one secret or a reference, and cannot decode
          // fields_mismatch, so it gets a reason it knows.
          for (const legacy of [
            { type: "save", secret: "pw" } as const,
            {
              type: "reference",
              reference: { provider: vault, reference: "fake://github/token" },
            } as const,
          ]) {
            const refused = yield* service
              .answer({ threadId, turnItemId, answer: legacy })
              .pipe(Effect.flip);
            assert.equal(refused.reason, "store_failed");
          }
          // A partial answer, or one with an extra value, is wrong too.
          for (const wrong of [
            answer({ email: "qa@acme.dev" }),
            answer({ email: "qa@acme.dev", password: "pw", extra: "x" }),
          ]) {
            assert.equal((yield* wrong.pipe(Effect.flip)).reason, "fields_mismatch");
          }
          assert.deepEqual(valuesOf(stored), []);
          // A failed record removes every field's value, not just the first.
          const failed = yield* answer({ email: "qa@acme.dev", password: "pw" }).pipe(Effect.flip);
          assert.equal(failed.reason, "record_failed");
          assert.deepEqual(valuesOf(stored), []);
          // A value the cleanup could not remove is replaced by the next answer, never kept.
          failNextRemove();
          yield* answer({ email: "old@acme.dev", password: "old-pw" }).pipe(Effect.flip);
          assert.lengthOf(valuesOf(stored), 1);

          yield* answer({ email: "qa@acme.dev", password: "hunter2" });
          assert.include(dispatched[0] as object, { secretStatus: "saved" });
          assert.deepEqual((dispatched[0] as { fields?: unknown }).fields, [
            { name: "email", label: "Email" },
            { name: "password", label: "Password" },
          ]);
          assert.notInclude(JSON.stringify(dispatched), "hunter2");
          const saved = yield* service.saveToFile({ threadId, turnItemId });
          assert.isTrue("files" in saved);
          const files = "files" in saved ? saved.files : {};
          assert.deepEqual(Object.keys(files), ["email", "password"]);
          assert.equal(yield* fs.readFileString(files.email ?? ""), "qa@acme.dev");
          assert.equal(yield* fs.readFileString(files.password ?? ""), "hunter2");
          assert.deepEqual(valuesOf(stored), []);
        }),
      {
        directory,
        failedRecords: 2,
        fields: [
          { name: "email", label: "Email" },
          { name: "password", label: "Password" },
        ],
      },
    );
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("two racing answers to a fields card never mix their values", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    yield* withService(
      ({ service }) =>
        Effect.gen(function* () {
          const answer = (who: string) =>
            service.answer({
              threadId,
              turnItemId,
              answer: { type: "save", values: { email: `${who}@acme.dev`, password: `${who}-pw` } },
            });
          const results = yield* Effect.forEach([answer("a"), answer("b")], Effect.result, {
            concurrency: "unbounded",
          });
          assert.deepEqual(results.map((result) => result._tag).toSorted(), ["Failure", "Success"]);
          const saved = yield* service.saveToFile({ threadId, turnItemId });
          const files = "files" in saved ? saved.files : {};
          const email = yield* fs.readFileString(files.email ?? "");
          const password = yield* fs.readFileString(files.password ?? "");
          assert.equal(password, `${email.split("@")[0]}-pw`);
        }),
      {
        directory,
        fields: [
          { name: "email", label: "Email" },
          { name: "password", label: "Password" },
        ],
      },
    );
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a card waiting on a locked secret provider does not hold up another thread's card", () =>
  Effect.gen(function* () {
    const readStarted = yield* Deferred.make<void>();
    const unlocked = yield* Deferred.make<void>();
    const { layer } = fakeVault(
      Deferred.succeed(readStarted, undefined).pipe(Effect.andThen(Deferred.await(unlocked))),
    );
    yield* withService(
      ({ service, dispatched }) =>
        Effect.gen(function* () {
          const waiting = yield* service
            .answer({
              threadId,
              turnItemId,
              answer: {
                type: "reference",
                reference: { provider: vault, reference: "fake://github/token" },
              },
            })
            .pipe(Effect.forkChild);
          yield* Deferred.await(readStarted);
          yield* service.answer({
            threadId: ThreadId.make("thread-other"),
            turnItemId,
            answer: { type: "decline" },
          });
          assert.include(dispatched[0] as object, { secretStatus: "declined" });
          yield* Deferred.succeed(unlocked, undefined);
          yield* Fiber.join(waiting).pipe(Effect.ignore);
        }),
      { resolver: layer },
    );
  }),
);

it.effect("a fields answer interrupted between two values keeps neither", () =>
  Effect.gen(function* () {
    const firstStored = yield* Deferred.make<void>();
    yield* withService(
      ({ service, stored }) =>
        Effect.gen(function* () {
          const fiber = yield* service
            .answer({
              threadId,
              turnItemId,
              answer: { type: "save", values: { email: "a@acme.dev", password: "a-pw" } },
            })
            .pipe(Effect.forkChild);
          yield* Deferred.await(firstStored);
          yield* Fiber.interrupt(fiber);
          assert.deepEqual(valuesOf(stored), []);
        }),
      {
        afterStore: Deferred.succeed(firstStored, undefined).pipe(Effect.andThen(Effect.never)),
        fields: [
          { name: "email", label: "Email" },
          { name: "password", label: "Password" },
        ],
      },
    );
  }),
);

it.effect("a failed file write keeps the value, so the request can retry", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    yield* withService(
      ({ service, stored }) =>
        Effect.gen(function* () {
          yield* service.answer({
            threadId,
            turnItemId,
            answer: { type: "save", secret: "cf_token" },
          });
          const name = Array.from(stored.keys())
            .map((key) => /^secret-request-([0-9a-f]{32})$/.exec(key)?.[1])
            .find((match) => match !== undefined)!;
          // A non-empty directory where the partial file goes makes the write fail.
          const partial = path.join(directory, "files", `${name}.partial`);
          yield* fs.makeDirectory(path.join(partial, "block"), { recursive: true });
          const failed = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
          assert.equal(failed.reason, "store_failed");
          yield* fs.remove(partial, { recursive: true });
          const file = singleFile(yield* service.saveToFile({ threadId, turnItemId }));
          assert.equal(yield* fs.readFileString(file), "cf_token");
        }),
      { directory },
    );
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a file is not delivered while its stored copy cannot be removed", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    yield* withService(
      ({ service }) =>
        Effect.gen(function* () {
          yield* service.answer({
            threadId,
            turnItemId,
            answer: { type: "save", secret: "cf_token" },
          });
          const first = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
          assert.equal(first.reason, "consume_failed");
          // A retry finds the file but still will not hand it out until the removal works.
          const retried = yield* service.saveToFile({ threadId, turnItemId }).pipe(Effect.flip);
          assert.equal(retried.reason, "consume_failed");
        }),
      { directory, removeFails: true },
    );
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a pasted secret goes straight to an owner-only file, with no card", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    const spans: Array<Tracer.NativeSpan> = [];
    const tracer = Tracer.make({
      span: (options) => {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    });
    const pasted = "4829'$(id) 16";
    const file = yield* withService(
      ({ service, stored, dispatched }) =>
        Effect.gen(function* () {
          const file = yield* service.savePasted({
            threadId,
            requestNamespace: "session-a",
            requestKey: "key-1",
            value: pasted,
          });
          assert.equal(path.dirname(file), path.join(directory, "files"));
          assert.match(path.basename(file), /^[0-9a-f]{32}$/);
          assert.equal(yield* fs.readFileString(file), pasted);
          if ((yield* HostProcess.Platform) !== "win32") {
            assert.equal((yield* fs.stat(file)).mode & 0o777, 0o600);
            assert.equal((yield* fs.stat(path.dirname(file))).mode & 0o777, 0o700);
          }
          // No card and no stored copy: the thread never hears of it.
          assert.deepEqual(dispatched, []);
          assert.deepEqual(valuesOf(stored), []);
          // A retry with the same key gets the same file, unchanged even if the value differs.
          assert.equal(
            yield* service.savePasted({
              threadId,
              requestNamespace: "session-a",
              requestKey: "key-1",
              value: "other",
            }),
            file,
          );
          assert.equal(yield* fs.readFileString(file), pasted);
          // Another key, or the same key from another provider session, gets its own file.
          const other = yield* service.savePasted({
            threadId,
            requestNamespace: "session-a",
            requestKey: "key-2",
            value: "x",
          });
          assert.notEqual(other, file);
          const otherSession = yield* service.savePasted({
            threadId,
            requestNamespace: "session-b",
            requestKey: "key-1",
            value: "y",
          });
          assert.notEqual(otherSession, file);
          assert.equal(yield* fs.readFileString(file), pasted);
          return file;
        }).pipe(Effect.withTracer(tracer)),
      { directory },
    );
    const recorded = spans.flatMap((span) => [
      span.name,
      ...Array.from(span.attributes.values(), String),
    ]);
    assert.include(recorded, "SecretRequests.savePasted");
    assert.isFalse(recorded.some((value) => value.includes("4829")));

    const { mtime } = yield* fs.stat(file);
    yield* TestClock.setTime(Option.getOrThrow(mtime).getTime() + SECRET_FILE_TTL_MS);
    yield* withService(() => Effect.void, { directory });
    assert.isTrue(yield* fs.exists(file));
    yield* TestClock.setTime(Option.getOrThrow(mtime).getTime() + SECRET_FILE_TTL_MS + 1);
    yield* withService(() => Effect.void, { directory });
    assert.isFalse(yield* fs.exists(file));
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a pasted secret keeps its exact bytes from the tool input to the file", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const directory = yield* fs.makeTempDirectoryScoped({ prefix: "secret-files-test-" });
    const pasted = "  pa ss\tword\t ";
    const input = yield* decodeSecretInput({
      label: "Staging password",
      reason: "Signs in the QA account.",
      value: pasted,
    });
    assert.equal(input.value, pasted);
    const empty = decodeSecretInputExit({
      label: "Staging password",
      reason: "Signs in the QA account.",
      value: "",
    });
    assert.isTrue(Exit.isFailure(empty));
    for (const url of ["https://?state=x", "javascript:alert(1)"]) {
      assert.isTrue(
        Exit.isFailure(decodeSecretInputExit({ label: "Code", reason: "Sign in.", url })),
        url,
      );
    }
    const file = yield* withService(
      ({ service }) =>
        service.savePasted({
          threadId,
          requestNamespace: "session-a",
          requestKey: "key-1",
          value: input.value!,
        }),
      { directory },
    );
    assert.equal(yield* fs.readFileString(file), pasted);
  }).pipe(Effect.provide(NodeServices.layer)),
);

it.effect("a pasted secret is refused without a secrets directory", () =>
  withService(({ service }) =>
    Effect.gen(function* () {
      const failed = yield* service
        .savePasted({ threadId, requestNamespace: "session-a", requestKey: "key-1", value: "4829" })
        .pipe(Effect.flip);
      assert.equal(failed.reason, "store_failed");
    }),
  ),
);
