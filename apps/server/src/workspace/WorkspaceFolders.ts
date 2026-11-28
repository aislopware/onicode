import {
  PROJECT_TRACKED_FOLDERS_MAX,
  type ProjectId,
  type ProjectTrackedFolder,
  type TaskFolder,
  type WorkspaceFolder,
  WorkspaceFoldersError,
  type WorkspaceListFoldersInput,
  type WorkspaceListFoldersResult,
  type WorkspaceScanFoldersInput,
  type WorkspaceScanFoldersResult,
} from "@onicode/contracts";
import { expandHomePath } from "@onicode/provider-core/server/pathExpansion";
import { splitsSurrogatePair } from "@onicode/shared/assistantCitations";
import * as HostProcess from "@onicode/shared/HostProcess";
import { fromLenientJson } from "@onicode/shared/schemaJson";
import { ONICODE_PROJECT_FILE_NAME } from "@onicode/contracts";
import { parseOniProjectFile } from "@onicode/shared/oniProjectFile";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import { isWithinRoot } from "./KnownWorkspaces.ts";
import { MAX_PENDING_LINK_TARGET_PROBES, probeFolder, resolveLinkTarget } from "./linkTargets.ts";
import { networkPathReachedBy } from "./networkLinks.ts";
import * as TaskFolderStore from "./TaskFolderStore.ts";

/** A tracked folder a thread works in, besides its own cwd. */
export interface ThreadFolder {
  readonly name: string;
  readonly path: string;
  readonly isGitRepository: boolean;
  /** The folder `path` is a checkout of. Absent when `path` is that folder. */
  readonly repositoryPath?: string;
}

export interface ThreadWorkspace {
  readonly folders: ReadonlyArray<ThreadFolder>;
  /** Paths of the folders outside the thread's cwd, which an agent needs granted to reach. */
  readonly extraRoots: ReadonlyArray<string>;
}

const EMPTY_WORKSPACE: ThreadWorkspace = { folders: [], extraRoots: [] };
// Each one costs a status per turn.
const THREAD_SUBMODULES_MAX = 32;

export class WorkspaceFolders extends Context.Service<
  WorkspaceFolders,
  {
    /**
     * The tracked folders a thread reaches. A thread in a task folder reaches that folder's
     * worktrees; one in a single worktree reaches none, since the project's checkouts are shared.
     */
    readonly forThread: (input: {
      readonly projectId: ProjectId;
      readonly worktreePath: string | null;
    }) => Effect.Effect<ThreadWorkspace, WorkspaceFoldersError>;
    /**
     * Submodules checked out inside the thread's cwd or its folders. Their repository owns how
     * they get there, so they are reviewed like the folders but never granted or announced.
     */
    readonly submodules: (input: {
      readonly projectId: ProjectId;
      readonly worktreePath: string | null;
      /** The thread's `forThread` folders; passed in so the project is walked once. */
      readonly folders: ReadonlyArray<ThreadFolder>;
    }) => Effect.Effect<ReadonlyArray<ThreadFolder>, WorkspaceFoldersError>;
    readonly list: (
      input: WorkspaceListFoldersInput,
    ) => Effect.Effect<WorkspaceListFoldersResult, WorkspaceFoldersError>;
    readonly scan: (
      input: WorkspaceScanFoldersInput,
    ) => Effect.Effect<WorkspaceScanFoldersResult, WorkspaceFoldersError>;
  }
>()("onicode/workspace/WorkspaceFolders") {}

const DETECTION_CONCURRENCY = 8;
// What `ProjectTrackedFolder.name` allows.
const FOLDER_NAME_MAX = 128;
const boundedName = (name: string) => {
  const trimmed = name.trimStart();
  const end = splitsSurrogatePair(trimmed, FOLDER_NAME_MAX) ? FOLDER_NAME_MAX - 1 : FOLDER_NAME_MAX;
  return trimmed.slice(0, end).trimEnd();
};
const CODE_WORKSPACE_EXTENSION = ".code-workspace";

const decodeCodeWorkspace = Schema.decodeUnknownEffect(
  fromLenientJson(
    Schema.Struct({
      folders: Schema.optional(
        Schema.Array(
          Schema.Struct({
            path: Schema.optional(Schema.String),
            name: Schema.optional(Schema.String),
          }),
        ),
      ),
    }),
  ),
);

const GIT_CONFIG_ESCAPES: Record<string, string> = { n: "\n", t: "\t", b: "\b" };
const GIT_CONFIG_SECTION = /^\s*\[((?:"(?:\\.|[^"\\])*"|[^\]"])*)\]/s;
const GIT_SUBMODULE_SECTION = /^submodule(?:\.([a-z0-9.-]*))?(?:\s+"((?:\\.|[^"\\])*)")?$/is;

/** Submodule paths a `.gitmodules` file declares, relative to its folder with `/` separators. */
export const parseGitmodulesPaths = (text: string): ReadonlyArray<string> => {
  const lines = text.split(/\r?\n/);
  const paths = new Map<string, string>();
  let submoduleName: string | undefined;
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
    // Git allows a key, or another section, on the same line after a section header.
    let rest = lines[lineIndex] ?? "";
    let header = GIT_CONFIG_SECTION.exec(rest);
    while (header !== null) {
      const [, dottedName, quotedName] = GIT_SUBMODULE_SECTION.exec(header[1] ?? "") ?? [];
      const dotted = dottedName?.toLowerCase();
      const quoted = quotedName?.replace(/\\(.)/gs, "$1");
      submoduleName =
        dotted !== undefined && quoted !== undefined ? `${dotted}.${quoted}` : (dotted ?? quoted);
      rest = rest.slice(header[0].length);
      header = GIT_CONFIG_SECTION.exec(rest);
    }
    const entry = /^\s*([a-z][a-z0-9-]*)\s*=(.*)$/is.exec(rest);
    if (entry === null) continue;
    // Git's value syntax: unquoted whitespace at either end is dropped, `#` or `;` outside quotes
    // starts a comment, and a backslash escapes a character or continues onto the next line,
    // so every key's value is read even though only `path` is kept.
    let raw = entry[2] ?? "";
    let value = "";
    let blank = "";
    let quoted = false;
    for (let index = 0; index < raw.length; index++) {
      const char = raw[index] ?? "";
      if (!quoted && " \t\r".includes(char)) {
        if (value !== "") blank += char;
        continue;
      }
      if (!quoted && (char === "#" || char === ";")) break;
      value += blank;
      blank = "";
      if (char === '"') quoted = !quoted;
      else if (char !== "\\") value += char;
      else if (index === raw.length - 1) raw += lines[++lineIndex] ?? "";
      else {
        const next = raw[++index] ?? "";
        value += GIT_CONFIG_ESCAPES[next] ?? next;
      }
    }
    if (submoduleName !== undefined && entry[1]?.toLowerCase() === "path") {
      paths.set(submoduleName, value.replace(/\/+$/, ""));
    }
  }
  return [...paths.values()].filter((path) => path !== "");
};

// A repository can link `.git`, `.gitmodules` or a folder onto a share, and Windows signs in to
// one as soon as stat or open reaches it. Anything leading to a share outside `root` is missing.
const reachesShare = (path: Path.Path, target: string, root: string) =>
  Effect.promise(() =>
    networkPathReachedBy(path, target, { allowedRoot: root }).then(
      (reached) => reached !== null,
      () => true,
    ),
  );

// Built once per operation: the probes share the caller's file system and path services.
const folderProbes = Effect.gen(function* () {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;

  const exists = (target: string) =>
    fileSystem.exists(target).pipe(Effect.orElseSucceed(() => false));

  const existsWithin = (target: string, root: string) =>
    reachesShare(path, target, root).pipe(
      Effect.flatMap((reached) => (reached ? Effect.succeed(false) : exists(target))),
    );

  const readWithin = (file: string, root: string) =>
    reachesShare(path, file, root).pipe(
      Effect.flatMap((reached) =>
        reached
          ? Effect.succeed("")
          : fileSystem.readFileString(file).pipe(Effect.orElseSucceed(() => "")),
      ),
    );

  // Sandboxes and containment checks work on what a path resolves to, not on a symlink's name.
  const realPath = (target: string) =>
    fileSystem.realPath(target).pipe(Effect.orElseSucceed(() => target));

  // A repository is a folder with its own `.git`; a folder deeper inside one is plain files.
  // Untraced: it runs once per folder inside the calling operation's span.
  const classify = Effect.fnUntraced(function* (
    folder: ProjectTrackedFolder,
    root: string,
  ): Effect.fn.Return<WorkspaceFolder> {
    if (yield* existsWithin(path.join(folder.path, ".git"), root)) {
      return { ...folder, state: "git" };
    }
    return { ...folder, state: (yield* existsWithin(folder.path, root)) ? "plain" : "missing" };
  });

  // A linked worktree and a submodule keep `.git` as a file; both follow another repository.
  const ownsHistory = (folder: string, root: string) => {
    const git = path.join(folder, ".git");
    return reachesShare(path, git, root).pipe(
      Effect.flatMap((reached) =>
        reached
          ? Effect.succeed(false)
          : fileSystem.stat(git).pipe(
              Effect.map((info) => info.type === "Directory"),
              Effect.orElseSucceed(() => false),
            ),
      ),
    );
  };

  // A link to a stalled network mount would hang every probe below it and hold libuv's workers.
  // One that does not answer in time is left out of this scan; a later scan finds it again.
  const linksToFolder = (target: string, root: string) =>
    resolveLinkTarget(target, { link: true, within: { path, root } }).pipe(
      Effect.map((linked) => linked?.kind === "directory"),
    );

  const childFolders = (folder: string, root: string) =>
    fileSystem.readDirectoryEntries(folder).pipe(
      Effect.orElseSucceed((): ReadonlyArray<FileSystem.DirectoryEntry> => []),
      Effect.flatMap((entries) =>
        Effect.filter(
          entries
            .filter(({ name }) => !name.startsWith(".") && name !== "node_modules")
            .toSorted((left, right) =>
              left.name < right.name ? -1 : left.name > right.name ? 1 : 0,
            ),
          ({ name, type }) =>
            type === "SymbolicLink"
              ? linksToFolder(path.join(folder, name), root)
              : Effect.succeed(true),
          { concurrency: MAX_PENDING_LINK_TARGET_PROBES },
        ),
      ),
      Effect.map((children) => children.map(({ name }) => path.join(folder, name))),
    );

  return {
    exists,
    existsWithin,
    readWithin,
    realPath,
    classify,
    ownsHistory,
    linksToFolder,
    childFolders,
  };
});

const readCodeWorkspaceFolders = Effect.fn("WorkspaceFolders.readCodeWorkspaceFolders")(function* (
  root: string,
  fileName: string,
) {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const file = path.join(root, fileName);
  if (yield* reachesShare(path, file, root)) return null;
  const document = yield* fileSystem
    .readFileString(file)
    .pipe(Effect.flatMap(decodeCodeWorkspace), Effect.option);
  if (Option.isNone(document)) return null;
  return (document.value.folders ?? []).flatMap((folder) => {
    const folderPath = folder.path;
    if (folderPath === undefined || folderPath === "") return [];
    const resolved = path.resolve(root, folderPath);
    if (resolved !== resolved.trim()) return [];
    return [
      {
        path: resolved,
        name: boundedName(folder.name?.trim() ?? "") || boundedName(path.basename(resolved)),
      },
    ];
  });
});

/**
 * Finds the repositories under `workspaceRoot` and the folders its `.code-workspace` file lists.
 */
const scanFolders = Effect.fn("WorkspaceFolders.scan")(function* (
  input: WorkspaceScanFoldersInput,
  limit: number = PROJECT_TRACKED_FOLDERS_MAX,
): Effect.fn.Return<
  WorkspaceScanFoldersResult,
  WorkspaceFoldersError,
  FileSystem.FileSystem | Path.Path
> {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const { existsWithin, readWithin, realPath, classify, ownsHistory, linksToFolder, childFolders } =
    yield* folderProbes;
  const root = path.resolve(
    expandHomePath(input.workspaceRoot.trim(), yield* HostProcess.HomeDirectory),
  );
  const entries = yield* fileSystem
    .readDirectoryEntries(root)
    .pipe(
      Effect.mapError(
        () => new WorkspaceFoldersError({ operation: "scan", detail: `Could not read ${root}.` }),
      ),
    );
  const names = entries.map((entry) => entry.name).toSorted();
  const links = new Set(
    entries.flatMap((entry) => (entry.type === "SymbolicLink" ? [entry.name] : [])),
  );
  const codeWorkspaceFile = names.find((name) => name.endsWith(CODE_WORKSPACE_EXTENSION)) ?? null;
  const listed =
    codeWorkspaceFile === null ? null : yield* readCodeWorkspaceFolders(root, codeWorkspaceFile);
  // A worktree of another repository is that repository's, so only folders with their own
  // history are offered. A plain child is looked into once: wrappers keep repositories under
  // folders such as `src` or `packages`.
  const children = yield* Effect.forEach(
    names.filter((name) => !name.startsWith(".") && name !== "node_modules"),
    (name) => {
      const folderPath = path.join(root, name);
      return Effect.gen(function* () {
        if (links.has(name) && !(yield* linksToFolder(folderPath, root))) return [];
        if (yield* ownsHistory(folderPath, root)) {
          return [{ path: folderPath, name: boundedName(name) }];
        }
        if (yield* existsWithin(path.join(folderPath, ".git"), root)) return [];
        const nested = yield* Effect.filter(
          yield* childFolders(folderPath, root),
          (nestedPath) => ownsHistory(nestedPath, root),
          { concurrency: DETECTION_CONCURRENCY },
        );
        return nested.map((nestedPath) => ({
          path: nestedPath,
          name: boundedName(path.basename(nestedPath)),
        }));
      });
    },
    { concurrency: DETECTION_CONCURRENCY },
  );
  const submodules = new Set(
    parseGitmodulesPaths(yield* readWithin(path.join(root, ".gitmodules"), root)),
  );
  const realRoot = yield* realPath(root);
  const resolved = yield* Effect.forEach(
    [
      ...(listed ?? []).map((folder) => ({ ...folder, source: "code-workspace" as const })),
      ...children.flat().map((folder) => ({ ...folder, source: "child" as const })),
    ],
    (folder) =>
      Effect.gen(function* () {
        if (folder.source === "child")
          return [{ folder, real: yield* realPath(folder.path), state: undefined }];
        // A workspace file can list a folder on a stalled mount; like a link, one that does not
        // answer in time is left out of this scan.
        const probed = yield* probeFolder(folder.path, { path, root });
        return probed === undefined ? [] : [{ folder, real: probed.realPath, state: probed.state }];
      }),
    { concurrency: DETECTION_CONCURRENCY },
  );
  const seen = new Set<string>();
  const candidates = resolved.flat().flatMap(({ folder, real, state }) => {
    // The scanned folder and its parents are never tracked, however a symlink names them.
    if (seen.has(real) || isWithinRoot(path, realRoot, real)) return [];
    seen.add(real);
    // Submodules follow their parent repository.
    return submodules.has(path.relative(root, folder.path).split(path.sep).join("/"))
      ? []
      : [{ folder, state }];
  });
  // Capped after the missing ones are dropped: dead entries must not hide the folders that exist.
  const folders = yield* Effect.forEach(
    candidates,
    ({ folder, state }) =>
      (state === undefined
        ? classify(folder, root).pipe(Effect.map((classified) => classified.state))
        : Effect.succeed(state)
      ).pipe(
        Effect.map((found) =>
          found === "missing" ? [] : [{ ...folder, isGitRepository: found === "git" }],
        ),
      ),
    { concurrency: DETECTION_CONCURRENCY },
  );
  return {
    workspaceRoot: root,
    isGitRepository: yield* existsWithin(path.join(root, ".git"), root),
    codeWorkspaceFile:
      listed === null || codeWorkspaceFile === null ? null : path.join(root, codeWorkspaceFile),
    folders: folders.flat().slice(0, limit),
  };
});

/**
 * The repositories inside a project folder, read from disk: the ones a scan finds there, less
 * those the project's `onicode.json` leaves out. A folder outside it is never found this way, since
 * tracking one grants agents a place they could not reach before. Null when the folder cannot be
 * read, which says nothing about what is inside it.
 */
export const discoverFolders = Effect.fn("WorkspaceFolders.discover")(function* (
  workspaceRoot: string,
): Effect.fn.Return<
  ReadonlyArray<ProjectTrackedFolder> | null,
  never,
  FileSystem.FileSystem | Path.Path
> {
  const path = yield* Path.Path;
  const { readWithin, realPath, ownsHistory } = yield* folderProbes;
  const root = path.resolve(expandHomePath(workspaceRoot.trim(), yield* HostProcess.HomeDirectory));
  const ignored = (
    parseOniProjectFile(yield* readWithin(path.join(root, ONICODE_PROJECT_FILE_NAME), root))
      ?.ignoreRepositories ?? []
  ).map((entry) => path.resolve(root, entry));
  // Everything the scan could find would be dropped, so skip the walk.
  if (ignored.some((entry) => isWithinRoot(path, root, entry))) return [];
  // Uncapped: the folders outside the project are dropped below, and must not use up the room.
  const scan = yield* scanFolders({ workspaceRoot }, Number.POSITIVE_INFINITY).pipe(Effect.option);
  if (Option.isNone(scan)) return null;
  const realRoot = yield* realPath(root);
  const inside = yield* Effect.filter(
    scan.value.folders.filter(
      (folder) =>
        folder.isGitRepository &&
        !ignored.some((entry) => folder.path === entry || isWithinRoot(path, folder.path, entry)),
    ),
    // A link to a repository elsewhere is not inside the project, whatever its name says, and a
    // worktree a workspace file lists follows another repository.
    (folder) =>
      Effect.all([realPath(folder.path), ownsHistory(folder.path, root)]).pipe(
        Effect.map(
          ([real, owns]) => owns && real !== realRoot && isWithinRoot(path, real, realRoot),
        ),
      ),
    { concurrency: DETECTION_CONCURRENCY },
  );
  return inside.map((folder) => ({ path: folder.path, name: folder.name, discovered: true }));
});

/**
 * The found repositories, then the folders someone added. Someone chose the added ones, so the
 * found ones give way when both do not fit.
 */
export const mergeProjectFolders = (
  discovered: ReadonlyArray<ProjectTrackedFolder>,
  trackedFolders: ReadonlyArray<ProjectTrackedFolder>,
): ReadonlyArray<ProjectTrackedFolder> => {
  const found = new Set(discovered.map((folder) => folder.path));
  const added = trackedFolders
    .filter((folder) => folder.discovered !== true && !found.has(folder.path))
    .slice(0, PROJECT_TRACKED_FOLDERS_MAX);
  return [...discovered.slice(0, PROJECT_TRACKED_FOLDERS_MAX - added.length), ...added];
};

/**
 * Every folder a project works in besides its own: the repositories found inside it now, then
 * the folders someone added. Read from disk each time, so a clone or a removal shows at once.
 */
export const projectFolders = Effect.fn("WorkspaceFolders.projectFolders")(function* (project: {
  readonly workspaceRoot: string;
  readonly trackedFolders: ReadonlyArray<ProjectTrackedFolder>;
}): Effect.fn.Return<
  ReadonlyArray<ProjectTrackedFolder>,
  never,
  FileSystem.FileSystem | Path.Path
> {
  return mergeProjectFolders(
    // What could not be read keeps the repositories found there before.
    (yield* discoverFolders(project.workspaceRoot)) ??
      project.trackedFolders.filter((folder) => folder.discovered === true),
    project.trackedFolders,
  );
});

/** Whether two tracked lists name the same folders in the same order. */
export const sameProjectFolders = (
  left: ReadonlyArray<ProjectTrackedFolder>,
  right: ReadonlyArray<ProjectTrackedFolder>,
) =>
  left.length === right.length &&
  left.every((folder, index) => {
    const other = right[index]!;
    return (
      folder.path === other.path &&
      folder.name === other.name &&
      (folder.discovered === true) === (other.discovered === true)
    );
  });

const make = Effect.gen(function* () {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const projects = yield* ProjectStore.ProjectStoreV2;
  const taskFolders = yield* TaskFolderStore.TaskFolderStore;

  const { exists, existsWithin, readWithin, realPath } = yield* folderProbes;

  const readProject = Effect.fn("WorkspaceFolders.readProject")(function* (projectId: ProjectId) {
    const row = yield* projects
      .get(projectId)
      .pipe(
        Effect.mapError(
          () => new WorkspaceFoldersError({ operation: "list", detail: "Failed to read project." }),
        ),
      );
    if (Option.isNone(row)) {
      return yield* new WorkspaceFoldersError({
        operation: "list",
        detail: `Project ${projectId} was not found.`,
      });
    }
    return row.value;
  });

  // Any tracked folder, or a link to one inside the project, can sit on a stalled mount, and
  // counts as missing for a call it does not answer in time. `real` is set for a present folder.
  const classifyAll = (folders: ReadonlyArray<ProjectTrackedFolder>) =>
    Effect.forEach(
      folders,
      (folder) =>
        probeFolder(folder.path).pipe(
          Effect.map((probed) => ({
            folder: { ...folder, state: probed?.state ?? "missing" } satisfies WorkspaceFolder,
            real: probed?.realPath,
          })),
        ),
      { concurrency: MAX_PENDING_LINK_TARGET_PROBES },
    );
  const foldersOf = (project: {
    readonly workspaceRoot: string;
    readonly trackedFolders: ReadonlyArray<ProjectTrackedFolder>;
  }) =>
    projectFolders(project).pipe(
      Effect.provideService(FileSystem.FileSystem, fileSystem),
      Effect.provideService(Path.Path, path),
    );

  // `identity` is `root` in the checkout the thread's was made from: it names the same submodule
  // in every worktree. `budget` is shared by one listing: a `.gitmodules` file is untrusted, and
  // one that repeats a path or links back to its parent would otherwise be walked without end.
  const submodulesBelow = (
    root: { readonly path: string; readonly identity: string },
    budget: { left: number; readonly visited: Set<string> },
  ): Effect.Effect<ReadonlyArray<ThreadFolder>> =>
    readWithin(path.join(root.path, ".gitmodules"), root.path).pipe(
      Effect.flatMap((text) =>
        Effect.forEach(
          [...new Set(parseGitmodulesPaths(text))],
          Effect.fnUntraced(function* (relative) {
            const folder = path.resolve(root.path, relative);
            const identity = path.resolve(root.identity, relative);
            if (folder === root.path || !isWithinRoot(path, folder, root.path)) return [];
            // One that was never initialized is an empty folder.
            if (budget.left <= 0 || !(yield* existsWithin(path.join(folder, ".git"), root.path))) {
              return [];
            }
            const real = yield* realPath(folder);
            // A committed symlink on the way must not lead the thread out of its repository.
            if (!isWithinRoot(path, real, yield* realPath(root.path))) return [];
            if (budget.left <= 0 || budget.visited.has(real)) return [];
            budget.visited.add(real);
            budget.left -= 1;
            return [
              {
                name: path.basename(folder),
                path: folder,
                isGitRepository: true,
                ...(identity === folder ? {} : { repositoryPath: identity }),
              },
              ...(yield* submodulesBelow({ path: folder, identity }, budget)),
            ];
          }),
          { concurrency: DETECTION_CONCURRENCY },
        ),
      ),
      Effect.map((found) => found.flat()),
    );

  const submodulesOf = Effect.fnUntraced(function* (
    roots: ReadonlyArray<{ readonly path: string; readonly identity: string }>,
  ) {
    const budget = {
      left: THREAD_SUBMODULES_MAX,
      visited: new Set(
        yield* Effect.forEach(roots, (root) => realPath(root.path), {
          concurrency: DETECTION_CONCURRENCY,
        }),
      ),
    };
    const found = yield* Effect.forEach(roots, (root) => submodulesBelow(root, budget), {
      concurrency: DETECTION_CONCURRENCY,
    });
    const taken = new Set(roots.map((root) => root.path));
    return found.flat().filter((folder) => !taken.has(folder.path));
  });

  // A tracked repository an agent checked out where the task folder would have put it, after the
  // folder was laid out. Only those inside the thread's own folder count: they need no grant.
  const checkedOutSince = Effect.fnUntraced(function* (cwd: string, taskFolder: TaskFolder) {
    const project = yield* readProject(taskFolder.projectId).pipe(Effect.option);
    if (Option.isNone(project)) return [];
    const recorded = new Set(
      taskFolder.repositories.flatMap((repository) => [
        repository.repositoryPath,
        repository.worktreePath,
      ]),
    );
    const found = yield* Effect.forEach(
      yield* foldersOf(project.value),
      Effect.fnUntraced(function* (folder) {
        if (recorded.has(folder.path) || !isWithinRoot(path, folder.path, taskFolder.anchor))
          return [];
        const checkout = path.join(taskFolder.root, path.relative(taskFolder.anchor, folder.path));
        if (recorded.has(checkout) || !(yield* exists(path.join(checkout, ".git")))) return [];
        const real = yield* realPath(checkout);
        if (real === cwd || !isWithinRoot(path, real, cwd)) return [];
        return [
          { name: folder.name, path: checkout, isGitRepository: true, repositoryPath: folder.path },
        ];
      }),
      { concurrency: DETECTION_CONCURRENCY },
    );
    return found.flat();
  });

  // A task folder's worktrees stand in for the tracked folders: the thread is told about, granted
  // and checkpoints only those, never the checkouts they were made from.
  const forTaskFolder = Effect.fnUntraced(function* (worktreePath: string, taskFolder: TaskFolder) {
    const cwd = yield* realPath(worktreePath);
    const recorded = taskFolder.repositories.map((repository) => ({
      name: repository.name,
      path: repository.worktreePath,
      isGitRepository: true,
      repositoryPath: repository.repositoryPath,
    }));
    const folders = [...recorded, ...(yield* checkedOutSince(cwd, taskFolder))];
    const outside = yield* Effect.forEach(
      folders,
      (folder) =>
        realPath(folder.path).pipe(
          Effect.map((real) => (isWithinRoot(path, real, cwd) ? [] : [folder.path, real])),
        ),
      { concurrency: DETECTION_CONCURRENCY },
    );
    return { folders, extraRoots: [...new Set(outside.flat())] };
  });

  const forThread: WorkspaceFolders["Service"]["forThread"] = Effect.fn(
    "WorkspaceFolders.forThread",
  )(function* (input) {
    if (input.worktreePath !== null) {
      const taskFolder = yield* taskFolders
        .findContaining(input.worktreePath)
        .pipe(
          Effect.mapError(
            (cause) => new WorkspaceFoldersError({ operation: "list", detail: cause.detail }),
          ),
        );
      return taskFolder === null
        ? EMPTY_WORKSPACE
        : yield* forTaskFolder(input.worktreePath, taskFolder);
    }
    const project = yield* readProject(input.projectId);
    const tracked = yield* foldersOf(project);
    if (tracked.length === 0) return EMPTY_WORKSPACE;
    const present = (yield* classifyAll(tracked)).filter(
      ({ folder }) => folder.state !== "missing",
    );
    const folders = present.map(({ folder }) => ({
      name: folder.name,
      path: folder.path,
      isGitRepository: folder.state === "git",
    }));
    const projectRoot = yield* realPath(project.workspaceRoot);
    // Both spellings are granted: agents are told the stored path, sandboxes check the real one.
    const outside = present.map(({ folder, real = folder.path }) =>
      isWithinRoot(path, real, projectRoot) ? [] : [folder.path, real],
    );
    return { folders, extraRoots: [...new Set(outside.flat())] };
  });

  const submodules: WorkspaceFolders["Service"]["submodules"] = Effect.fn(
    "WorkspaceFolders.submodules",
  )(function* (input) {
    // A worktree can outlive its project record; its submodules are then named by where they are.
    const projectRoot = yield* readProject(input.projectId).pipe(
      Effect.map((project) => path.resolve(project.workspaceRoot)),
      Effect.option,
    );
    const own = input.worktreePath === null ? null : path.resolve(input.worktreePath);
    const root = own ?? Option.getOrNull(projectRoot);
    if (root === null) return [];
    const identity = Option.getOrElse(projectRoot, () => root);
    return yield* submodulesOf([
      { path: root, identity },
      ...input.folders.flatMap((folder) =>
        folder.isGitRepository
          ? [{ path: folder.path, identity: folder.repositoryPath ?? folder.path }]
          : [],
      ),
    ]);
  });

  const list: WorkspaceFolders["Service"]["list"] = Effect.fn("WorkspaceFolders.list")(
    function* (input) {
      const project = yield* readProject(input.projectId);
      const classified = yield* classifyAll(yield* foldersOf(project));
      return { folders: classified.map(({ folder }) => folder) };
    },
  );

  const scan: WorkspaceFolders["Service"]["scan"] = (input) =>
    scanFolders(input).pipe(
      Effect.provideService(FileSystem.FileSystem, fileSystem),
      Effect.provideService(Path.Path, path),
    );

  return WorkspaceFolders.of({ forThread, submodules, list, scan });
});

export const layer = Layer.effect(WorkspaceFolders, make);
