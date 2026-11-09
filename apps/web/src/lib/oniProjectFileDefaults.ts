import {
  ONICODE_PROJECT_FILE_NAME,
  type EnvironmentId,
  type OniProjectFile,
} from "@onicode/contracts";
import { parseOniProjectFile } from "@onicode/shared/oniProjectFile";
import { executeAtomQuery } from "@onicode/client-runtime/state/runtime";

import {
  getProjectFileQueryAtom,
  resolveProjectFileQueryData,
} from "~/components/files/projectFilesQueryState";
import { appAtomRegistry } from "~/rpc/atomRegistry";

export async function readOniProjectFile(
  environmentId: EnvironmentId,
  workspaceRoot: string,
): Promise<OniProjectFile | null> {
  const result = await executeAtomQuery(
    appAtomRegistry,
    getProjectFileQueryAtom(environmentId, workspaceRoot, ONICODE_PROJECT_FILE_NAME),
    { reportDefect: false, reportFailure: false },
  );
  const data = resolveProjectFileQueryData(
    environmentId,
    workspaceRoot,
    ONICODE_PROJECT_FILE_NAME,
    result._tag === "Success" ? result.value : null,
  );
  if (data === null || data.exists === false || data.truncated) return null;
  return parseOniProjectFile(data.contents);
}
