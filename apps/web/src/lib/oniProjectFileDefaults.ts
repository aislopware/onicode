import {
  ONICODE_PROJECT_FILE_NAME,
  type EnvironmentId,
  type ProjectReadFileResult,
  type OniProjectFile,
} from "@onicode/contracts";
import { parseOniProjectFile } from "@onicode/shared/oniProjectFile";
import { PROJECT_READ_FILE_STALE_TIME_MS } from "@onicode/client-runtime/state/projects";
import { executeAtomQuery } from "@onicode/client-runtime/state/runtime";
import { AsyncResult } from "effect/reactivity";

import {
  getProjectFileQueryAtom,
  resolveProjectFileQueryData,
} from "~/components/files/projectFilesQueryState";
import { appAtomRegistry } from "~/rpc/atomRegistry";

function toOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
  result: ProjectReadFileResult | null,
): OniProjectFile | null {
  const data = resolveProjectFileQueryData(
    environmentId,
    workspaceRoot,
    ONICODE_PROJECT_FILE_NAME,
    result,
  );
  if (data === null || data.exists === false || data.truncated) return null;
  return parseOniProjectFile(data.contents);
}

/** With `refresh`, a cached copy does not answer: the read waits for the file again. */
export async function readOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
  options: { readonly refresh?: boolean; readonly signal?: AbortSignal | undefined } = {},
): Promise<OniProjectFile | null> {
  const result = await executeAtomQuery(
    appAtomRegistry,
    getProjectFileQueryAtom(environmentId, workspaceRoot, ONICODE_PROJECT_FILE_NAME),
    {
      reportDefect: false,
      reportFailure: false,
      refresh: options.refresh ?? false,
      signal: options.signal,
    },
  );
  return toOniProjectFile(
    environmentId,
    workspaceRoot,
    result._tag === "Success" ? result.value : null,
  );
}

/**
 * The project's onicode.json from an earlier read, or `undefined` when none has loaded yet. A
 * copy that is refetching or past the query's stale time, so the file may have changed, is not
 * `current`.
 */
export function peekOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
): { readonly projectFile: OniProjectFile | null; readonly current: boolean } | undefined {
  const result = appAtomRegistry.get(
    getProjectFileQueryAtom(environmentId, workspaceRoot, ONICODE_PROJECT_FILE_NAME),
  );
  return AsyncResult.isSuccess(result)
    ? {
        projectFile: toOniProjectFile(environmentId, workspaceRoot, result.value),
        current: !result.waiting && Date.now() - result.timestamp < PROJECT_READ_FILE_STALE_TIME_MS,
      }
    : undefined;
}
