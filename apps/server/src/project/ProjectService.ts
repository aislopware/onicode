import * as NodeUtil from "node:util";
import {
  CommandId,
  type OrchestrationProjectShell,
  PROJECT_TRACKED_FOLDERS_MAX,
  ProjectId,
  type Project,
  type ProjectCreatePayload,
  type ProjectTrackedFolder,
  type ProjectUpdatePayload,
  type ProjectSnapshot,
  ThreadId,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as KeyedLock from "@onicode/shared/KeyedLock";
import * as Clock from "effect/Clock";
import { isWslSharePath } from "@onicode/shared/path";
import { resolveProjectScripts } from "@onicode/shared/projectScripts";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";

import { withCommandMetrics } from "../observability/Metrics.ts";
import type * as CommandReceiptStore from "../orchestration-v2/CommandReceiptStore.ts";
import * as EventSink from "../orchestration-v2/EventSink.ts";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import * as LegacyV1ThreadImporter from "../orchestration-v2/legacy/LegacyV1ThreadImporter.ts";
import * as ProjectionStore from "../orchestration-v2/ProjectionStore.ts";
import {
  decodeProjectCommandRejection,
  encodeProjectCommandRejection,
  planProjectCommand,
  type ProjectCommand,
} from "../orchestration-v2/ProjectCommands.ts";
import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as ThreadCommandExecutor from "../orchestration-v2/ThreadCommandExecutor.ts";
import { planThreadDeletion } from "../orchestration-v2/ThreadDeletion.ts";
import * as ServerSettings from "../serverSettings.ts";
import { deletedProjectOverridePatch } from "../provider/ProjectSettingsOverrideSweep.ts";
import { makeIsManagedWorktreePath } from "../worktreesDirectory.ts";
import * as ProcessRunner from "../processRunner.ts";
import * as TerminalManager from "../terminal/Manager.ts";
import * as ProjectEnrichmentService from "./ProjectEnrichmentService.ts";
import { makeIsWorktreeContainer } from "./worktreeContainer.ts";
import { isWithinRoot } from "../workspace/KnownWorkspaces.ts";
import {
  discoverFolders,
  mergeProjectFolders,
  projectFolders,
  sameProjectFolders,
} from "../workspace/WorkspaceFolders.ts";
import * as WorkspacePaths from "../workspace/WorkspacePaths.ts";

export interface ProjectCreateInput extends ProjectCreatePayload {
  readonly commandId: CommandId;
  readonly projectId: ProjectId;
}

export interface ProjectUpdateInput extends ProjectUpdatePayload {
  readonly commandId: CommandId;
  readonly projectId: ProjectId;
  /** Added to, and removed by path from, the list the project has when the update commits. */
  readonly trackFolders?: ReadonlyArray<ProjectTrackedFolder> | undefined;
  readonly untrackFolders?: ReadonlyArray<string> | undefined;
}

export interface ProjectBootstrapInput extends ProjectCreateInput {}

export interface ProjectDeleteInput {
  readonly commandId: CommandId;
  readonly projectId: ProjectId;
  readonly force?: boolean;
}

export class ProjectNotFoundError extends Schema.TaggedError<ProjectNotFoundError>()(
  "ProjectNotFoundError",
  { projectId: ProjectId },
) {
  override get message(): string {
    return `Project ${this.projectId} was not found.`;
  }
}

export class ProjectConflictError extends Schema.TaggedError<ProjectConflictError>()(
  "ProjectConflictError",
  {
    projectId: ProjectId,
    workspaceRoot: Schema.String,
    conflictingProjectId: ProjectId,
  },
) {
  override get message(): string {
    return `Workspace ${this.workspaceRoot} already belongs to project ${this.conflictingProjectId}.`;
  }
}

export class ProjectNotEmptyError extends Schema.TaggedError<ProjectNotEmptyError>()(
  "ProjectNotEmptyError",
  { projectId: ProjectId },
) {
  override get message(): string {
    return `Project ${this.projectId} is not empty.`;
  }
}

/** A change the project's rules refuse, such as an invalid script ID: the caller's input is wrong, nothing failed. */
export class ProjectInvalidError extends Schema.TaggedError<ProjectInvalidError>()(
  "ProjectInvalidError",
  { projectId: ProjectId, detail: Schema.String },
) {
  override get message(): string {
    return this.detail;
  }
}

export class ProjectOperationError extends Schema.TaggedError<ProjectOperationError>()(
  "ProjectOperationError",
  {
    operation: Schema.Literals([
      "normalize-workspace",
      "read-project",
      "list-projects",
      "list-threads",
      "delete-thread",
      "dispatch-project-command",
      "write-project-settings",
    ]),
    projectId: Schema.optional(ProjectId),
    workspaceRoot: Schema.optional(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Project operation '${this.operation}' failed${this.projectId === undefined ? "" : ` for ${this.projectId}`}.`;
  }
}

const isProjectOperationError = Schema.is(ProjectOperationError);

export type ProjectServiceError =
  | ProjectNotFoundError
  | ProjectConflictError
  | ProjectNotEmptyError
  | ProjectInvalidError
  | ProjectOperationError;

export class ProjectService extends Context.Service<
  ProjectService,
  {
    readonly create: (input: ProjectCreateInput) => Effect.Effect<Project, ProjectServiceError>;
    readonly bootstrap: (
      input: ProjectBootstrapInput,
    ) => Effect.Effect<
      { readonly project: Project; readonly created: boolean },
      ProjectServiceError
    >;
    readonly update: (input: ProjectUpdateInput) => Effect.Effect<Project, ProjectServiceError>;
    readonly delete: (input: ProjectDeleteInput) => Effect.Effect<Project, ProjectServiceError>;
    readonly getById: (
      projectId: ProjectId,
      options?: { readonly includeDeleted?: boolean },
    ) => Effect.Effect<Option.Option<Project>, ProjectOperationError>;
    readonly getByWorkspaceRoot: (
      workspaceRoot: string,
      options?: { readonly includeDeleted?: boolean },
    ) => Effect.Effect<Option.Option<Project>, ProjectOperationError>;
    /**
     * Where to scan `cwd`'s workspace inventory. A thread's removed managed worktree scans its
     * project, which turn start recreates the worktree from; anything else, or a failure, is `cwd`.
     */
    readonly workspaceScanDirectory: (cwd: string) => Effect.Effect<string>;
    readonly snapshot: Effect.Effect<ProjectSnapshot, ProjectOperationError>;
    /**
     * A page of active projects in creation order, each with its repository
     * identity resolved, so a caller can match projects by repository.
     */
    readonly listPage: (input: {
      readonly cursor: number;
      readonly limit: number;
    }) => Effect.Effect<
      { readonly projects: ReadonlyArray<Project>; readonly nextCursor: number | null },
      ProjectOperationError
    >;
    readonly getShell: (
      projectId: ProjectId,
    ) => Effect.Effect<Option.Option<OrchestrationProjectShell>, ProjectOperationError>;
    readonly listShells: (options?: {
      readonly projectIds?: ReadonlyArray<ProjectId>;
    }) => Effect.Effect<ReadonlyArray<OrchestrationProjectShell>, ProjectOperationError>;
  }
>()("onicode/project/ProjectService") {}

const FOLDER_SYNC_INTERVAL_MS = 30_000;

export const make = Effect.gen(function* () {
  const projects = yield* ProjectStore.ProjectStoreV2;
  const projectEnrichment = yield* ProjectEnrichmentService.ProjectEnrichmentService;
  const workspacePaths = yield* WorkspacePaths.WorkspacePaths;
  const threadProjections = yield* ProjectionStore.ProjectionStoreV2;
  const eventSink = yield* EventSink.EventSinkV2;
  const idAllocator = yield* IdAllocator.IdAllocatorV2;
  const legacyImporter = yield* LegacyV1ThreadImporter.LegacyV1ThreadImporter;
  const threadCommands = yield* ThreadCommandExecutor.ThreadCommandExecutor;
  const serverSettings = yield* ServerSettings.ServerSettingsService;
  // Terminals live in the server process; the CLI builds this service without them.
  const terminals = yield* Effect.serviceOption(TerminalManager.TerminalManager);
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const isManagedWorktreePath = yield* makeIsManagedWorktreePath;
  const isWorktreeContainer = yield* makeIsWorktreeContainer.pipe(
    Effect.provide(ProcessRunner.layer),
  );
  const scope = yield* Effect.scope;
  const projectLocks = yield* KeyedLock.make<ProjectId>();
  const workspaceLocks = yield* KeyedLock.make<string>();

  const toProject = (
    row: ProjectStore.ProjectRow,
    enrichment: ProjectEnrichmentService.ProjectEnrichment | null,
  ): Project => ({
    id: row.projectId,
    title: row.title,
    workspaceRoot: row.workspaceRoot,
    repositoryIdentity: enrichment?.repositoryIdentity ?? null,
    faviconPath: row.faviconPath ?? enrichment?.faviconPath ?? null,
    defaultModelSelection: row.defaultModelSelection,
    defaultThreadEnvMode: row.defaultThreadEnvMode,
    autoPull: row.autoPull,
    projectIcon: row.projectIcon,
    scripts: row.scripts,
    trackedFolders: row.trackedFolders,
    pinnedAt: row.pinnedAt,
    archivedAt: row.archivedAt,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
    deletedAt: row.deletedAt,
  });

  // Clients read `trackedFolders` off the project, so the stored list is brought back in step
  // with the disk when a project is read, at most this often. Server work reads the disk itself.
  const folderSyncDue = new Map<ProjectId, number>();
  const syncFolders = Effect.fn("ProjectService.syncFolders")(function* (
    row: ProjectStore.ProjectRow,
  ) {
    const now = yield* Clock.currentTimeMillis;
    if ((folderSyncDue.get(row.projectId) ?? 0) > now) return;
    folderSyncDue.set(row.projectId, now + FOLDER_SYNC_INTERVAL_MS);
    const discovered = yield* discoverFolders(row.workspaceRoot).pipe(
      Effect.provideService(FileSystem.FileSystem, fileSystem),
      Effect.provideService(Path.Path, path),
    );
    // An unmounted or unreadable folder keeps what it tracks until it can be read again.
    if (discovered === null) return;
    if (sameProjectFolders(mergeProjectFolders(discovered, row.trackedFolders), row.trackedFolders))
      return;
    const command = {
      type: "project.meta.update",
      commandId: CommandId.make(`project-folders:${row.projectId}:${now}`),
      projectId: row.projectId,
    } as const;
    // `row` was read before the scan: an update since then may have added or removed a folder.
    yield* commit(command, {
      refine: (current) => {
        if (current?.deletedAt !== null || current.workspaceRoot !== row.workspaceRoot)
          return Effect.succeed(null);
        const trackedFolders = mergeProjectFolders(discovered, current.trackedFolders);
        return Effect.succeed(
          sameProjectFolders(trackedFolders, current.trackedFolders)
            ? null
            : { ...command, trackedFolders },
        );
      },
    });
  });

  const hydrate = Effect.fn("ProjectService.hydrate")(function* (row: ProjectStore.ProjectRow) {
    if (row.deletedAt === null) {
      yield* syncFolders(row).pipe(Effect.ignoreCause({ log: true }), Effect.forkIn(scope));
    }
    const enrichment =
      row.deletedAt === null
        ? yield* projectEnrichment.getAvailable(row.workspaceRoot)
        : yield* projectEnrichment.peek(row.workspaceRoot);
    return toProject(row, enrichment);
  });

  const readRow = (projectId: ProjectId, options?: { readonly includeDeleted?: boolean }) =>
    projects
      .get(projectId, options)
      .pipe(
        Effect.mapError(
          (cause) => new ProjectOperationError({ operation: "read-project", projectId, cause }),
        ),
      );

  const normalizeWorkspaceRoot = (input: {
    readonly projectId?: ProjectId;
    readonly workspaceRoot: string;
    readonly createIfMissing?: boolean;
  }) =>
    workspacePaths
      .normalizeWorkspaceRoot(input.workspaceRoot, {
        createIfMissing: input.createIfMissing ?? false,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ProjectOperationError({
              operation: "normalize-workspace",
              ...(input.projectId === undefined ? {} : { projectId: input.projectId }),
              workspaceRoot: input.workspaceRoot,
              cause,
            }),
        ),
      );

  // Paths elsewhere on the wire trim, so git and terminals would run in the neighbouring folder.
  const rejectUntrimmedWorkspaceRoot = (projectId: ProjectId, workspaceRoot: string) =>
    workspaceRoot.trim() === workspaceRoot
      ? Effect.void
      : Effect.fail(
          new ProjectInvalidError({
            projectId,
            detail: `Onicode cannot use a folder whose path starts or ends with a space: "${workspaceRoot}".`,
          }),
        );

  const rejectWorktreeContainer = Effect.fn("ProjectService.rejectWorktreeContainer")(function* (
    projectId: ProjectId,
    workspaceRoot: string,
  ) {
    if (!(yield* isWorktreeContainer(workspaceRoot))) return;
    return yield* new ProjectInvalidError({
      projectId,
      detail: `${workspaceRoot} holds a repository's worktrees but is not a checkout itself. Add one of its worktrees as the project instead.`,
    });
  });
  // A folder may be missing from disk: it shows as missing until it is restored or removed.
  const withDiscovered = (workspaceRoot: string, added: ReadonlyArray<ProjectTrackedFolder>) =>
    projectFolders({ workspaceRoot, trackedFolders: added }).pipe(
      Effect.provideService(FileSystem.FileSystem, fileSystem),
      Effect.provideService(Path.Path, path),
    );

  const normalizeTrackedFolders = Effect.fn("ProjectService.normalizeTrackedFolders")(function* (
    projectId: ProjectId,
    workspaceRoot: string,
    folders: ReadonlyArray<ProjectTrackedFolder>,
    known: ReadonlyArray<ProjectTrackedFolder> = [],
  ) {
    const invalid = (detail: string) => Effect.fail(new ProjectInvalidError({ projectId, detail }));
    const realPath = (target: string) =>
      fileSystem.realPath(target).pipe(Effect.orElseSucceed(() => target));
    // Compared by what a symlink points at, so an alias cannot pass for another folder.
    const realRoot = yield* realPath(workspaceRoot);
    const normalized: Array<ProjectTrackedFolder> = [];
    const seen = new Set<string>();
    // The server finds the repositories inside the project folder itself; callers add the rest.
    for (const folder of folders.filter((entry) => entry.discovered !== true)) {
      if (!path.isAbsolute(folder.path)) {
        return yield* invalid(`Tracked folder '${folder.path}' must be an absolute path.`);
      }
      const folderPath = path.resolve(folder.path);
      const real = yield* realPath(folderPath);
      if (isWithinRoot(path, workspaceRoot, folderPath) || isWithinRoot(path, realRoot, real)) {
        return yield* invalid(
          folderPath === workspaceRoot || real === realRoot
            ? "The project's own folder is always part of it; track other folders."
            : `Tracked folder '${folderPath}' contains the project's own folder.`,
        );
      }
      if (seen.has(real)) {
        return yield* invalid(`Tracked folder '${folderPath}' is listed twice.`);
      }
      seen.add(real);
      normalized.push({ path: folderPath, name: folder.name });
    }
    if (normalized.length > PROJECT_TRACKED_FOLDERS_MAX) {
      return yield* invalid(
        `A project tracks at most ${PROJECT_TRACKED_FOLDERS_MAX} added folders; ${normalized.length} were given.`,
      );
    }
    // Only a folder being added has to be there: one already tracked may have gone missing since.
    return yield* Effect.forEach(
      normalized.filter((folder) => !known.some((existing) => existing.path === folder.path)),
      (folder) =>
        fileSystem.stat(folder.path).pipe(
          Effect.map((info) => info.type === "Directory"),
          Effect.orElseSucceed(() => false),
          Effect.flatMap((isFolder) =>
            isFolder ? Effect.void : invalid(`'${folder.path}' is not a folder on this machine.`),
          ),
        ),
      { discard: true },
    ).pipe(Effect.andThen(withDiscovered(workspaceRoot, normalized)));
  });

  // Git for Windows can't trust an index written inside the distro and rereads every file across the
  // share, so git status can hit its timeout. Checked before normalizing, which already stats the share.
  const rejectWslSharePath = Effect.fn("ProjectService.rejectWslSharePath")(function* (
    projectId: ProjectId,
    workspaceRoot: string,
  ) {
    if (!isWslSharePath(workspaceRoot) || !(yield* HostProcess.isWindows)) return;
    return yield* new ProjectInvalidError({
      projectId,
      detail: `${workspaceRoot.trim()} is inside WSL. Add it to the WSL backend instead of the Windows one.`,
    });
  });

  const commit = Effect.fn("ProjectService.commit")(function* (
    initial: ProjectCommand,
    options?: {
      // Decides the command from the row as it is under the project's lock; null commits nothing.
      // It must keep the command's type and workspace root, which pick the locks.
      readonly refine?: (
        current: ProjectStore.ProjectRow | undefined,
      ) => Effect.Effect<ProjectCommand | null, ProjectServiceError>;
      // Runs under the project lock before a first commit: a failure commits nothing, so a retry
      // with the same command id runs it again, and a replayed receipt skips it.
      readonly beforeCommit?: Effect.Effect<void, ProjectOperationError> | undefined;
    },
  ) {
    const { projectId } = initial;
    const refine = options?.refine;
    const beforeCommit = options?.beforeCommit;
    const dispatchError = (cause: unknown) =>
      new ProjectOperationError({ operation: "dispatch-project-command", projectId, cause });
    const workspaceRoot = initial.type === "project.delete" ? undefined : initial.workspaceRoot;
    const planAndCommit = Effect.gen(function* () {
      // A retry gets the first outcome, though the row it would be refined against has moved on.
      const decided = yield* eventSink.projectCommandReceipt(initial.commandId);
      if (Option.isSome(decided)) return { receipt: decided.value };
      const project = Option.getOrUndefined(yield* readRow(projectId, { includeDeleted: true }));
      let command = initial;
      if (refine !== undefined) {
        const refined = yield* Effect.exit(refine(project));
        if (Exit.isFailure(refined)) return { refused: refined.cause };
        if (refined.value === null) return null;
        command = refined.value;
      }
      const workspaceOwner =
        workspaceRoot === undefined
          ? undefined
          : Option.getOrUndefined(
              yield* projects
                .findActiveByWorkspaceRoot(workspaceRoot)
                .pipe(Effect.mapError(dispatchError)),
            );
      const now = yield* DateTime.now;
      const eventId = yield* idAllocator.allocate
        .event({ commandId: command.commandId })
        .pipe(Effect.mapError(dispatchError));
      const planned = planProjectCommand({
        command,
        state: { project, workspaceOwner },
        eventId,
        now,
      });
      if (Result.isSuccess(planned)) {
        if (beforeCommit !== undefined) yield* beforeCommit;
        const { receipt } = yield* eventSink.commitProjectCommand({
          commandId: command.commandId,
          projectId,
          commandType: command.type,
          acceptedAt: now,
          event: planned.success,
        });
        return { receipt };
      }
      const receipt = yield* eventSink.commitRejectedProjectCommand({
        commandId: command.commandId,
        projectId,
        commandType: command.type,
        rejectedAt: now,
        error: encodeProjectCommandRejection(planned.failure),
      });
      return { receipt };
    });
    const outcome = yield* projectLocks
      .withLock(
        projectId,
        workspaceRoot === undefined
          ? planAndCommit
          : workspaceLocks.withLock(workspaceRoot, planAndCommit),
      )
      .pipe(
        Effect.mapError((error) =>
          isProjectOperationError(error) && error.operation === "write-project-settings"
            ? error
            : dispatchError(error),
        ),
      );
    if (outcome === null) return;
    if ("refused" in outcome) return yield* Effect.failCause(outcome.refused);
    return yield* settle(initial, outcome.receipt);
  }, withCommandMetrics);

  // The outcome a command's receipt records, as its first sender saw it.
  const settle = Effect.fnUntraced(function* (
    initial: Pick<ProjectCommand, "commandId" | "projectId" | "type">,
    receipt: CommandReceiptStore.ProjectCommandReceiptV2,
  ) {
    const { projectId } = initial;
    const dispatchError = (cause: unknown) =>
      new ProjectOperationError({ operation: "dispatch-project-command", projectId, cause });
    if (receipt.projectId !== projectId || receipt.commandType !== initial.type) {
      return yield* dispatchError(
        `Command ${initial.commandId} was already used by ${receipt.commandType} for ${receipt.projectId}.`,
      );
    }
    // Retries re-plan against their own effects; the first receipt determines the outcome.
    if (receipt.status === "accepted") return;
    const rejection = Option.getOrUndefined(decodeProjectCommandRejection(receipt.error));
    switch (rejection?._tag) {
      case "ProjectWorkspaceConflictError":
        return yield* new ProjectConflictError({
          projectId,
          workspaceRoot: rejection.workspaceRoot,
          conflictingProjectId: rejection.conflictingProjectId,
        });
      case "ProjectCommandMissingProjectError":
        return yield* new ProjectNotFoundError({ projectId });
      case "ProjectCommandInvariantError":
        return yield* new ProjectInvalidError({ projectId, detail: rejection.detail });
      default:
        return yield* dispatchError(
          rejection ?? receipt.error ?? "The command was previously rejected.",
        );
    }
  });

  // A retry gets its first outcome before anything is checked again: the folders it named may be
  // gone since, and the project deleted.
  const replayed = Effect.fn("ProjectService.replayed")(function* (
    initial: Pick<ProjectCommand, "commandId" | "projectId" | "type">,
  ) {
    const receipt = yield* eventSink.projectCommandReceipt(initial.commandId).pipe(
      Effect.mapError(
        (cause) =>
          new ProjectOperationError({
            operation: "dispatch-project-command",
            projectId: initial.projectId,
            cause,
          }),
      ),
    );
    if (Option.isNone(receipt)) return false;
    yield* settle(initial, receipt.value);
    return true;
  });

  // A first attempt may have stopped between its commit and its metadata refresh.
  const refreshReplayed = Effect.fn("ProjectService.refreshReplayed")(function* (
    projectId: ProjectId,
  ) {
    const row = yield* readRow(projectId, { includeDeleted: true });
    if (Option.isSome(row)) yield* projectEnrichment.invalidate([row.value.workspaceRoot]);
  });

  const readCommitted = Effect.fn("ProjectService.readCommitted")(function* (projectId: ProjectId) {
    const row = yield* readRow(projectId, { includeDeleted: true });
    if (Option.isNone(row)) {
      return yield* new ProjectOperationError({
        operation: "read-project",
        projectId,
        cause: "The accepted project command did not produce a project row.",
      });
    }
    return yield* hydrate(row.value);
  });

  const getById: ProjectService["Service"]["getById"] = Effect.fn("ProjectService.getById")(
    function* (projectId, options) {
      const row = yield* readRow(projectId, options);
      return Option.isNone(row) ? Option.none() : Option.some(yield* hydrate(row.value));
    },
  );

  const workspaceScanDirectory = Effect.fn("ProjectService.workspaceScanDirectory")(
    function* (cwd: string) {
      if (yield* fileSystem.exists(cwd).pipe(Effect.orElseSucceed(() => true))) return cwd;
      if (!(yield* isManagedWorktreePath(cwd))) return cwd;
      const snapshot = yield* Effect.flatten(
        threadProjections.readShellSnapshot({ worktreePath: cwd }),
      );
      const thread = [...snapshot.threads, ...snapshot.archivedThreads].find(
        (candidate) => candidate.branch !== null,
      );
      if (thread === undefined) return cwd;
      return Option.getOrUndefined(yield* readRow(thread.projectId))?.workspaceRoot ?? cwd;
    },
    (effect, cwd) => Effect.orElseSucceed(effect, () => cwd),
  );

  const getByWorkspaceRoot: ProjectService["Service"]["getByWorkspaceRoot"] = Effect.fn(
    "ProjectService.getByWorkspaceRoot",
  )(function* (workspaceRoot, options) {
    const normalized = yield* normalizeWorkspaceRoot({ workspaceRoot });
    const row = yield* (
      options?.includeDeleted === true
        ? projects
            .list({ includeDeleted: true })
            .pipe(
              Effect.map((rows) =>
                Option.fromUndefinedOr(rows.find((row) => row.workspaceRoot === normalized)),
              ),
            )
        : projects.findActiveByWorkspaceRoot(normalized)
    ).pipe(
      Effect.mapError((cause) => new ProjectOperationError({ operation: "list-projects", cause })),
    );
    return Option.isNone(row) ? Option.none() : Option.some(yield* hydrate(row.value));
  });

  // New threads resolve these from the settings overrides; once folded, the record is never read.
  const writeProjectSettings = (
    projectId: ProjectId,
    input: Pick<
      ProjectUpdatePayload,
      "defaultModelSelection" | "defaultThreadEnvMode" | "scripts" | "autoPull"
    >,
    previousScripts?: ProjectStore.ProjectRow["scripts"],
  ) => {
    if (
      input.defaultModelSelection === undefined &&
      input.defaultThreadEnvMode === undefined &&
      input.scripts === undefined &&
      input.autoPull === undefined
    )
      return undefined;
    return Effect.gen(function* () {
      const { scripts } = input;
      // A caller echoing the scripts it read, the project's own or the environment's it
      // inherits, must not flip inheriting into owning them or into choosing none.
      const echoesReadScripts =
        scripts !== undefined &&
        previousScripts !== undefined &&
        (yield* serverSettings.getSettings.pipe(
          Effect.map((settings) =>
            [settings, { ...settings, defaultProjectScripts: [] }].some((read) =>
              NodeUtil.isDeepStrictEqual(
                scripts,
                resolveProjectScripts(read, { id: projectId, scripts: previousScripts }),
              ),
            ),
          ),
        ));
      yield* serverSettings.updateProjectSettingsOverride(projectId, {
        defaultModelSelection: input.defaultModelSelection,
        defaultThreadEnvMode: input.defaultThreadEnvMode,
        // Like empty scripts on the legacy record, an empty list uses the environment's.
        defaultProjectScripts:
          scripts === undefined || echoesReadScripts
            ? undefined
            : scripts.length === 0
              ? null
              : scripts,
        defaultAutoPull: input.autoPull,
      });
    }).pipe(
      Effect.mapError(
        (cause) =>
          new ProjectOperationError({ operation: "write-project-settings", projectId, cause }),
      ),
      Effect.withSpan("ProjectService.writeProjectSettings"),
    );
  };

  const create: ProjectService["Service"]["create"] = Effect.fn("ProjectService.create")(
    function* (input) {
      if (
        yield* replayed({
          commandId: input.commandId,
          projectId: input.projectId,
          type: "project.create",
        })
      ) {
        yield* refreshReplayed(input.projectId);
        return yield* readCommitted(input.projectId);
      }
      yield* rejectUntrimmedWorkspaceRoot(input.projectId, input.workspaceRoot);
      // `foo /` and `foo /.` resolve to `foo `, which normalizing may create.
      yield* rejectUntrimmedWorkspaceRoot(input.projectId, path.resolve(input.workspaceRoot));
      yield* rejectWslSharePath(input.projectId, input.workspaceRoot);
      const workspaceRoot = yield* normalizeWorkspaceRoot({
        projectId: input.projectId,
        workspaceRoot: input.workspaceRoot,
        createIfMissing: input.createWorkspaceRootIfMissing ?? false,
      });
      yield* rejectUntrimmedWorkspaceRoot(input.projectId, workspaceRoot);
      yield* rejectWorktreeContainer(input.projectId, workspaceRoot);
      const trackedFolders = yield* normalizeTrackedFolders(
        input.projectId,
        workspaceRoot,
        input.trackedFolders ?? [],
      );
      yield* commit(
        {
          type: "project.create",
          commandId: input.commandId,
          projectId: input.projectId,
          title: input.title,
          workspaceRoot,
          ...(input.scripts === undefined ? {} : { scripts: input.scripts }),
          trackedFolders,
        },
        {
          beforeCommit: writeProjectSettings(input.projectId, {
            // A new project has nothing to clear.
            defaultModelSelection: input.defaultModelSelection ?? undefined,
            scripts: input.scripts?.length === 0 ? undefined : input.scripts,
          }),
        },
      );
      yield* projectEnrichment.invalidate([workspaceRoot]);
      return yield* readCommitted(input.projectId);
    },
  );

  const update: ProjectService["Service"]["update"] = Effect.fn("ProjectService.update")(
    function* (input) {
      if (
        yield* replayed({
          commandId: input.commandId,
          projectId: input.projectId,
          type: "project.meta.update",
        })
      ) {
        // The folder it moved from is no longer known.
        if (input.workspaceRoot !== undefined || input.faviconPath !== undefined)
          yield* refreshReplayed(input.projectId);
        return yield* readCommitted(input.projectId);
      }
      const existing = yield* readRow(input.projectId);
      if (Option.isNone(existing)) {
        return yield* new ProjectNotFoundError({ projectId: input.projectId });
      }
      const previousRoot = existing.value.workspaceRoot;
      if (input.workspaceRoot !== undefined) {
        yield* rejectUntrimmedWorkspaceRoot(input.projectId, input.workspaceRoot);
        if (input.workspaceRoot !== previousRoot) {
          yield* rejectWslSharePath(input.projectId, input.workspaceRoot);
        }
      }
      const workspaceRoot =
        input.workspaceRoot === undefined
          ? previousRoot
          : yield* normalizeWorkspaceRoot({
              projectId: input.projectId,
              workspaceRoot: input.workspaceRoot,
            });
      if (workspaceRoot !== previousRoot) {
        yield* rejectUntrimmedWorkspaceRoot(input.projectId, workspaceRoot);
        yield* rejectWorktreeContainer(input.projectId, workspaceRoot);
      }
      const command = {
        type: "project.meta.update",
        commandId: input.commandId,
        projectId: input.projectId,
        ...(input.title === undefined ? {} : { title: input.title }),
        ...(workspaceRoot === previousRoot ? {} : { workspaceRoot }),
        ...(input.defaultModelSelection === undefined
          ? {}
          : { defaultModelSelection: input.defaultModelSelection }),
        ...(input.autoPull === undefined ? {} : { autoPull: input.autoPull }),
        ...(input.projectIcon === undefined ? {} : { projectIcon: input.projectIcon }),
        ...(input.faviconPath === undefined ? {} : { faviconPath: input.faviconPath }),
        ...(input.defaultThreadEnvMode === undefined
          ? {}
          : { defaultThreadEnvMode: input.defaultThreadEnvMode }),
        ...(input.scripts === undefined ? {} : { scripts: input.scripts }),
        ...(input.pinned === undefined ? {} : { pinned: input.pinned }),
        ...(input.archived === undefined ? {} : { archived: input.archived }),
      } as const;
      // Checked against the row under the lock: an update that ran meanwhile may have moved the
      // project's folder or changed what it tracks.
      yield* commit(command, {
        refine: Effect.fnUntraced(function* (current) {
          if (current === undefined || current.deletedAt !== null) return command;
          const root = command.workspaceRoot ?? current.workspaceRoot;
          // Moving the project's folder re-checks the folders it keeps tracking.
          const untracked = new Set(
            (input.untrackFolders ?? []).map((entry) => path.resolve(entry)),
          );
          const tracking = input.trackFolders ?? [];
          if (
            input.trackedFolders === undefined &&
            untracked.size === 0 &&
            tracking.length === 0 &&
            root === current.workspaceRoot
          )
            return command;
          const listed: ReadonlyArray<ProjectTrackedFolder> =
            input.trackedFolders ?? current.trackedFolders;
          const kept = listed.filter(
            (folder) => folder.discovered !== true && !untracked.has(path.resolve(folder.path)),
          );
          const trackedFolders = yield* normalizeTrackedFolders(
            input.projectId,
            root,
            [
              ...kept,
              ...tracking.filter(
                (folder) =>
                  !kept.some((entry) => path.resolve(entry.path) === path.resolve(folder.path)),
              ),
            ],
            current.trackedFolders,
          );
          return { ...command, trackedFolders };
        }),
        beforeCommit: writeProjectSettings(input.projectId, input, existing.value.scripts),
      });
      if (workspaceRoot !== previousRoot || input.faviconPath !== undefined) {
        yield* projectEnrichment.invalidate([previousRoot, workspaceRoot]);
      }
      return yield* readCommitted(input.projectId);
    },
  );

  const bootstrap: ProjectService["Service"]["bootstrap"] = Effect.fn("ProjectService.bootstrap")(
    function* (input) {
      const existing = yield* getByWorkspaceRoot(input.workspaceRoot);
      if (Option.isSome(existing)) return { project: existing.value, created: false };
      return { project: yield* create(input), created: true };
    },
  );

  const deleteChildThread = Effect.fn("ProjectService.deleteChildThread")(function* (
    input: ProjectDeleteInput,
    threadId: ThreadId,
  ) {
    yield* legacyImporter.ensureTranscript(threadId);
    const projection = yield* threadProjections.getThreadRecords(threadId, [
      "runs",
      "attempts",
      "nodes",
      "runtimeRequests",
      "subagents",
      "providerSessions",
    ]);
    if (projection.thread.deletedAt !== null || projection.thread.projectId !== input.projectId) {
      return;
    }
    const command = {
      type: "thread.delete" as const,
      commandId: CommandId.make(`${input.commandId}:delete-thread:${threadId}`),
      threadId,
    };
    const now = yield* DateTime.now;
    const plan = yield* planThreadDeletion({
      command,
      projection,
      attachmentIds: yield* threadProjections.getThreadAttachmentIds(threadId),
      now,
      idAllocator,
    });
    const committed = yield* eventSink.commitCommand({
      commandId: command.commandId,
      commandType: command.type,
      threadId,
      acceptedAt: now,
      events: plan.events,
      effects: plan.effects,
    });
    if (
      committed.receipt.threadId !== command.threadId ||
      committed.receipt.commandType !== command.type
    ) {
      return yield* Effect.fail("The thread deletion command ID belongs to a different command.");
    }
    if (committed.receipt.status === "rejected") {
      return yield* Effect.fail(
        committed.receipt.error ?? "Thread deletion was previously rejected.",
      );
    }
  });

  const deleteChildThreads = Effect.fn("ProjectService.deleteChildThreads")(function* (
    input: ProjectDeleteInput,
  ) {
    const { projectId } = input;
    const snapshot = yield* threadProjections
      .getShellSnapshot({ projectId })
      .pipe(
        Effect.mapError(
          (cause) => new ProjectOperationError({ operation: "list-threads", projectId, cause }),
        ),
      );
    // Newest first: a source deleted while its fork lives keeps the pages that fork shows.
    const projectThreads = [...snapshot.threads, ...snapshot.archivedThreads].sort(
      (a, b) => DateTime.toEpochMillis(b.createdAt) - DateTime.toEpochMillis(a.createdAt),
    );
    if (projectThreads.length > 0 && input.force !== true) {
      return yield* new ProjectNotEmptyError({ projectId });
    }
    // Delete children durably before the project so a failed cascade can be retried.
    yield* Effect.forEach(
      projectThreads,
      (thread) =>
        threadCommands
          .withDeletionLock(threadCommands.withLock(thread.id, deleteChildThread(input, thread.id)))
          .pipe(
            Effect.mapError(
              (cause) =>
                new ProjectOperationError({ operation: "delete-thread", projectId, cause }),
            ),
          ),
      { concurrency: 1, discard: true },
    );
  });

  // Best-effort cleanup releases disabled instances kept alive by overrides; crashes have no replay to revisit deletion.
  const clearProjectSettingsOverrides = (projectId: ProjectId) =>
    serverSettings.getSettings.pipe(
      Effect.flatMap(({ projectSettingsOverrides }) =>
        serverSettings.updateSettings({
          projectSettingsOverrides: {
            [projectId]: deletedProjectOverridePatch(projectSettingsOverrides[projectId]),
          },
        }),
      ),
      Effect.asVoid,
      Effect.catchCause((cause) =>
        Effect.logWarning(
          `Failed to clear settings overrides for deleted project ${projectId}.`,
          cause,
        ),
      ),
    );

  // Draft threads have no row, so a deletion another client makes leaves their terminals open.
  const closeDraftThreadTerminals = Effect.fn("ProjectService.closeDraftThreadTerminals")(
    function* (projectId: ProjectId, workspaceRoot: string) {
      if (Option.isNone(terminals)) return;
      const manager = terminals.value;
      // A live project nested inside the deleted one keeps its own drafts.
      const nestedRoots = (yield* listRows).flatMap((row) =>
        row.deletedAt === null && isWithinRoot(path, row.workspaceRoot, workspaceRoot)
          ? [row.workspaceRoot]
          : [],
      );
      const threadIds = new Set(
        (yield* manager.list)
          .filter(({ projectId: owner, cwd, worktreePath }) =>
            owner !== null
              ? owner === projectId
              : // Older clients send no project. A draft on a worktree may then belong to
                // another project even when its folder is inside.
                worktreePath === null &&
                isWithinRoot(path, cwd, workspaceRoot) &&
                !nestedRoots.some((root) => isWithinRoot(path, cwd, root)),
          )
          .map((terminal) => terminal.threadId),
      );
      yield* Effect.forEach(
        threadIds,
        (threadId) =>
          Effect.suspend(() => threadProjections.findThread(ThreadId.make(threadId))).pipe(
            Effect.flatMap((thread) =>
              thread === null ? manager.close({ threadId, deleteHistory: true }) : Effect.void,
            ),
            Effect.catchCause((cause) =>
              Effect.logWarning(
                `Failed to close the terminals of draft thread ${threadId}.`,
                cause,
              ),
            ),
          ),
        { discard: true },
      );
    },
    Effect.catchCause((cause) =>
      Effect.logWarning("Failed to close draft thread terminals of a deleted project.", cause),
    ),
  );

  const deleteProject: ProjectService["Service"]["delete"] = Effect.fn("ProjectService.delete")(
    function* (input) {
      const { projectId } = input;
      // Read deleted rows so retries replay receipts and other command ids fail as not found.
      const existing = yield* readRow(projectId, { includeDeleted: true });
      if (Option.isNone(existing)) {
        return yield* new ProjectNotFoundError({ projectId });
      }

      const isFirstDelete = existing.value.deletedAt === null;
      if (isFirstDelete) {
        yield* deleteChildThreads(input);
      }
      yield* commit({ type: "project.delete", commandId: input.commandId, projectId });
      if (isFirstDelete) {
        yield* clearProjectSettingsOverrides(projectId);
      }
      yield* closeDraftThreadTerminals(projectId, existing.value.workspaceRoot);
      yield* projectEnrichment.invalidate([existing.value.workspaceRoot]);
      return yield* readCommitted(projectId);
    },
  );

  const enrichShell = (shell: OrchestrationProjectShell) =>
    projectEnrichment.getAvailable(shell.workspaceRoot).pipe(
      Effect.map((enrichment) => ({
        ...shell,
        repositoryIdentity: enrichment.repositoryIdentity,
      })),
    );

  const getShell: ProjectService["Service"]["getShell"] = Effect.fn("ProjectService.getShell")(
    function* (projectId) {
      const shell = yield* projects
        .getShell(projectId)
        .pipe(
          Effect.mapError(
            (cause) => new ProjectOperationError({ operation: "read-project", projectId, cause }),
          ),
        );
      return Option.isNone(shell) ? shell : Option.some(yield* enrichShell(shell.value));
    },
  );

  const listShells: ProjectService["Service"]["listShells"] = Effect.fn(
    "ProjectService.listShells",
  )(function* (options) {
    const shells = yield* projects
      .listShells(options)
      .pipe(
        Effect.mapError(
          (cause) => new ProjectOperationError({ operation: "list-projects", cause }),
        ),
      );
    return yield* Effect.forEach(shells, enrichShell, { concurrency: 16 });
  });

  const listRows = projects
    .list()
    .pipe(
      Effect.mapError((cause) => new ProjectOperationError({ operation: "list-projects", cause })),
    );

  const snapshot = Effect.gen(function* () {
    const rows = yield* listRows;
    const hydrated = yield* Effect.forEach(rows, hydrate, { concurrency: 8 });
    return {
      projects: hydrated,
      updatedAt: DateTime.formatIso(yield* DateTime.now),
    } satisfies ProjectSnapshot;
  });

  const listPage: ProjectService["Service"]["listPage"] = Effect.fn("ProjectService.listPage")(
    function* ({ cursor, limit }) {
      const rows = (yield* listRows).filter((project) => project.deletedAt === null);
      const projects = yield* Effect.forEach(
        rows.slice(cursor, cursor + limit),
        (row) =>
          hydrate(row).pipe(
            Effect.filterOrElse(
              (project) => project.repositoryIdentity !== null,
              (project) =>
                projectEnrichment
                  .readRepositoryIdentity(project.workspaceRoot)
                  .pipe(Effect.map((repositoryIdentity) => ({ ...project, repositoryIdentity }))),
            ),
          ),
        { concurrency: 8 },
      );
      return { projects, nextCursor: cursor + limit < rows.length ? cursor + limit : null };
    },
  );

  return ProjectService.of({
    create,
    bootstrap,
    update,
    delete: deleteProject,
    getById,
    getByWorkspaceRoot,
    workspaceScanDirectory,
    snapshot,
    listPage,
    getShell,
    listShells,
  });
});

export const layer = Layer.effect(ProjectService, make).pipe(
  Layer.provide(ThreadCommandExecutor.layer),
);
