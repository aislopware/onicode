import {
  AcpRegistryOperationError,
  CommandId,
  ThreadId,
  type AcpRegistryImportSessionInput,
  type AcpRegistryImportSessionResult,
  type Project,
  type ProjectId,
  type ProviderDriverKind,
  type ProviderInstanceId,
} from "@onicode/contracts";
import * as AcpRegistryRuntimeCoordinator from "@onicode/provider-acp-registry/server/AcpRegistryRuntimeCoordinator";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import type { ProviderInstance } from "@onicode/provider-core/server/driver";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Result from "effect/Result";
import * as SqlClient from "effect/sql/SqlClient";

import * as ThreadLaunchService from "../orchestration-v2/ThreadLaunchService.ts";
import * as ThreadManagementService from "../orchestration-v2/ThreadManagementService.ts";
import * as AgentSessionImporter from "../project/AgentSessionImporter.ts";
import * as ProjectService from "../project/ProjectService.ts";
import * as ServerRuntimeStartup from "../serverRuntimeStartup.ts";
import * as ProviderInstanceRegistry from "./ProviderInstanceRegistry.ts";
import * as ProviderRegistry from "./ProviderRegistry.ts";

export const importedThreadId = (input: {
  readonly driver: ProviderDriverKind;
  readonly instanceId: ProviderInstanceId;
  readonly sessionId: string;
}) =>
  IdAllocator.deriveThreadFromProviderThread({
    driver: input.driver,
    providerInstanceId: input.instanceId,
    nativeThreadId: input.sessionId,
  });

export class AcpRegistrySessions extends Context.Service<
  AcpRegistrySessions,
  {
    readonly project: (projectId: ProjectId) => Effect.Effect<Project, AcpRegistryOperationError>;
    readonly sessionManager: (instanceId: ProviderInstanceId) => Effect.Effect<
      {
        readonly instance: ProviderInstance;
        readonly manager: NonNullable<ProviderInstance["acpSessionManagement"]>;
      },
      AcpRegistryOperationError
    >;
    /** The thread that already runs each native session, for the sessions that have one. */
    readonly findOwners: (input: {
      readonly driver: ProviderDriverKind;
      readonly instanceId: ProviderInstanceId;
      readonly sessionIds: ReadonlyArray<string>;
    }) => Effect.Effect<ReadonlyMap<string, ThreadId>, AcpRegistryOperationError>;
    /**
     * Binds a native ACP session to a Onicode thread and copies its history in. Importing a
     * session whose thread still lacks its history repairs that thread instead.
     */
    readonly importSession: (
      input: AcpRegistryImportSessionInput,
    ) => Effect.Effect<AcpRegistryImportSessionResult, AcpRegistryOperationError>;
  }
>()("onicode/provider/AcpRegistrySessions") {}

const make = Effect.gen(function* () {
  const projectService = yield* ProjectService.ProjectService;
  const providerInstances = yield* ProviderInstanceRegistry.ProviderInstanceRegistry;
  const providerRegistry = yield* ProviderRegistry.ProviderRegistry;
  const coordinator = yield* AcpRegistryRuntimeCoordinator.AcpRegistryRuntimeCoordinator;
  const threadManagement = yield* ThreadManagementService.ThreadManagementService;
  const threadLaunch = yield* ThreadLaunchService.ThreadLaunchService;
  const startup = yield* ServerRuntimeStartup.ServerRuntimeStartup;
  const agentSessionImporter = yield* AgentSessionImporter.AgentSessionImporter;
  const random = yield* SecureRandom.SecureRandom;
  const sql = yield* SqlClient.SqlClient;

  // One query per listed page: the native ID lives in JSON, so each lookup scans the instance's rows.
  const findOwners = Effect.fn("AcpRegistrySessions.findOwners")(function* (input: {
    readonly driver: ProviderDriverKind;
    readonly instanceId: ProviderInstanceId;
    readonly sessionIds: ReadonlyArray<string>;
  }) {
    const owners = new Map<string, ThreadId>();
    if (input.sessionIds.length === 0) return owners;
    const rows = yield* sql<{ readonly native_id: string; readonly thread_id: string }>`
      SELECT
        json_extract(provider.payload_json, '$.nativeThreadRef.nativeId') AS native_id,
        provider.thread_id
      FROM orchestration_v2_projection_provider_threads AS provider
      JOIN orchestration_v2_projection_threads AS thread ON thread.thread_id = provider.thread_id
      WHERE provider.driver = ${input.driver}
        AND provider.provider_instance_id = ${input.instanceId}
        AND json_extract(provider.payload_json, '$.nativeThreadRef.nativeId') IN ${sql.in(input.sessionIds)}
        AND thread.deleted_at IS NULL
      ORDER BY thread.updated_at DESC
    `.pipe(
      Effect.mapError(
        (cause) =>
          new AcpRegistryOperationError({
            reason: "session_import_failed",
            message: "Could not inspect ACP session ownership.",
            cause,
          }),
      ),
    );
    for (const row of rows) {
      if (!owners.has(row.native_id)) owners.set(row.native_id, ThreadId.make(row.thread_id));
    }
    return owners;
  });
  const findOwner = (input: {
    readonly driver: ProviderDriverKind;
    readonly instanceId: ProviderInstanceId;
    readonly sessionId: string;
  }) =>
    findOwners({ ...input, sessionIds: [input.sessionId] }).pipe(
      Effect.map((owners) => owners.get(input.sessionId) ?? null),
    );

  const project = Effect.fn("AcpRegistrySessions.project")(function* (projectId: ProjectId) {
    const found = yield* projectService.getById(projectId).pipe(
      Effect.mapError(
        (cause) =>
          new AcpRegistryOperationError({
            reason: "project_not_found",
            message: `Project ${projectId} is unavailable.`,
            cause,
          }),
      ),
    );
    return yield* Effect.fromOption(
      found,
      () =>
        new AcpRegistryOperationError({
          reason: "project_not_found",
          message: `Project ${projectId} was not found.`,
        }),
    );
  });

  const sessionManager = Effect.fn("AcpRegistrySessions.sessionManager")(function* (
    instanceId: ProviderInstanceId,
  ) {
    const instance = yield* providerInstances.getInstance(instanceId);
    if (instance === undefined) {
      return yield* new AcpRegistryOperationError({
        reason: "instance_not_found",
        message: `Provider instance ${instanceId} was not found.`,
      });
    }
    if (instance.acpSessionManagement === undefined) {
      return yield* new AcpRegistryOperationError({
        reason: "session_list_unsupported",
        message: `Provider instance ${instanceId} does not expose ACP session management.`,
      });
    }
    return { instance, manager: instance.acpSessionManagement };
  });

  const importSession = Effect.fn("AcpRegistrySessions.importSession")(function* (
    input: AcpRegistryImportSessionInput,
  ) {
    const { workspaceRoot } = yield* project(input.projectId);
    const { instance } = yield* sessionManager(input.instanceId);
    const providerSnapshot = yield* instance.snapshot.getSnapshot;
    if (
      providerSnapshot.nativeSessions?.canLoad !== true &&
      providerSnapshot.nativeSessions?.canResume !== true
    ) {
      return yield* new AcpRegistryOperationError({
        reason: "session_resume_unsupported",
        message: "The ACP agent cannot load or resume native sessions.",
      });
    }
    const sessionKey = {
      driver: instance.driverKind,
      instanceId: input.instanceId,
      sessionId: input.sessionId,
    };
    const owner = yield* findOwner(sessionKey);
    const threadId = owner ?? importedThreadId(sessionKey);
    const existing = yield* threadManagement.getThreadShell(threadId).pipe(
      Effect.mapError(
        (cause) =>
          new AcpRegistryOperationError({
            reason: "session_import_failed",
            message: "Could not inspect the imported ACP session mapping.",
            cause,
          }),
      ),
    );
    if (existing !== null) {
      // A derived ID alone does not verify the native session binding for a cross-project repair.
      if (owner === null && existing.projectId !== input.projectId)
        return { threadId, imported: false } as const;
      const repairing = AgentSessionImporter.isNativeHistoryPending({
        historyOrigin: existing.historyOrigin,
        hasRun: existing.latestRunId !== null,
      });
      if (!repairing) return { threadId, imported: false } as const;
      yield* agentSessionImporter.importNativeHistory({ threadId, workspaceRoot }).pipe(
        Effect.mapError(
          (cause) =>
            new AcpRegistryOperationError({
              reason: "session_import_failed",
              message: "Could not import the ACP session history.",
              cause,
            }),
        ),
      );
      return { threadId, imported: true } as const;
    }

    const provider = (yield* providerRegistry.getProviders).find(
      (candidate) => candidate.instanceId === input.instanceId,
    );
    const model =
      provider?.models.find((candidate) => candidate.isDefault)?.slug ??
      provider?.models[0]?.slug ??
      "default";
    const commandId = CommandId.make(yield* random.uuidV4());
    const launched = yield* Effect.result(
      startup.enqueueCommand(
        threadLaunch.launch({
          commandId,
          projectId: input.projectId,
          title: input.title ?? "Imported ACP session",
          modelSelection: { instanceId: input.instanceId, model },
          runtimeMode: "approval-required",
          interactionMode: "default",
          workspaceStrategy: { type: "root" },
          importedNativeThread: {
            ref: {
              driver: instance.driverKind,
              nativeId: input.sessionId,
              strength: "strong",
            },
            metadata: {
              itemIdentityVersion: 2,
              ...(input.title === undefined ? {} : { title: input.title }),
              ...(input.updatedAt === undefined ? {} : { updatedAt: input.updatedAt }),
            },
          },
          createdBy: "user",
          creationSource: "web",
        }),
      ),
    );
    if (Result.isFailure(launched)) {
      const racedOwner = yield* findOwner(sessionKey).pipe(
        Effect.mapError(
          (cause) =>
            new AcpRegistryOperationError({
              reason: "session_import_failed",
              message: "Could not inspect the imported ACP session after launch failed.",
              cause,
            }),
        ),
      );
      if (racedOwner !== null) return { threadId: racedOwner, imported: false } as const;
      return yield* new AcpRegistryOperationError({
        reason: "session_import_failed",
        message: "Could not create an Onicode thread for the ACP session.",
        cause: launched.failure,
      });
    }
    const launchedThreadId = launched.success.threadId;
    // The thread is usable without its history, so a failed read only logs.
    yield* agentSessionImporter
      .importNativeHistory({ threadId: launchedThreadId, workspaceRoot })
      .pipe(
        Effect.catch((cause) =>
          Effect.logWarning("Could not import ACP session history", {
            instanceId: input.instanceId,
            sessionId: input.sessionId,
            cause,
          }),
        ),
      );
    return { threadId: launchedThreadId, imported: true } as const;
  }, coordinator.withSessionMutation);

  return AcpRegistrySessions.of({ project, sessionManager, findOwners, importSession });
});

export const layer = Layer.effect(AcpRegistrySessions, make);
