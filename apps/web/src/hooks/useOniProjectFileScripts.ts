import {
  ONICODE_PROJECT_FILE_NAME,
  type EnvironmentId,
  type OniProjectFile,
  type OniProjectFileScript,
} from "@onicode/contracts";
import { parseOniProjectFile } from "@onicode/shared/oniProjectFile";

import { useProjectFileQuery } from "~/components/files/projectFilesQueryState";

const NO_SCRIPTS: ReadonlyArray<OniProjectFileScript> = [];

export interface OniProjectFileState {
  status: "loading" | "missing" | "invalid" | "valid";
  file: OniProjectFile | null;
  scripts: ReadonlyArray<OniProjectFileScript>;
}

export function useOniProjectFileState(
  environmentId: EnvironmentId,
  cwd: string | null,
): OniProjectFileState {
  const query = useProjectFileQuery(
    environmentId,
    cwd ?? "",
    ONICODE_PROJECT_FILE_NAME,
    cwd !== null,
  );
  const contents = query.data && !query.data.truncated ? query.data.contents : null;
  const isPending = query.isPending;
  if (contents === null) {
    return {
      status: isPending ? "loading" : "missing",
      file: null,
      scripts: NO_SCRIPTS,
    } as const;
  }
  const file = parseOniProjectFile(contents);
  if (file === null) {
    return { status: "invalid", file: null, scripts: NO_SCRIPTS } as const;
  }
  return { status: "valid", file, scripts: file.scripts ?? NO_SCRIPTS } as const;
}

export function useOniProjectFileScripts(
  environmentId: EnvironmentId,
  cwd: string | null,
): ReadonlyArray<OniProjectFileScript> {
  return useOniProjectFileState(environmentId, cwd).scripts;
}
