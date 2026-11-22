import {
  ONICODE_PROJECT_FILE_NAME,
  type EnvironmentId,
  type ProjectReadFileResult,
  type OniProjectFile,
} from "@onicode/contracts";
import { parseOniProjectFile } from "@onicode/shared/oniProjectFile";
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

export async function readOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
): Promise<OniProjectFile | null> {
  const result = await executeAtomQuery(
    appAtomRegistry,
    getProjectFileQueryAtom(environmentId, workspaceRoot, ONICODE_PROJECT_FILE_NAME),
    { reportDefect: false, reportFailure: false },
  );
  return toOniProjectFile(
    environmentId,
    workspaceRoot,
    result._tag === "Success" ? result.value : null,
  );
}

/** The project's onicode.json from an earlier read, or `undefined` when none has loaded yet. */
export function peekOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
): OniProjectFile | null | undefined {
  const result = appAtomRegistry.get(
    getProjectFileQueryAtom(environmentId, workspaceRoot, ONICODE_PROJECT_FILE_NAME),
  );
  return AsyncResult.isSuccess(result)
    ? toOniProjectFile(environmentId, workspaceRoot, result.value)
    : undefined;
}
