import * as Dialog from "../../components/ui/Dialog";
import { errorDisplayMessage } from "@onicode/client-runtime/errors";
import { resolveAssetUrl } from "@onicode/client-runtime/state/assets";
import { executeAtomQuery } from "@onicode/client-runtime/state/runtime";
import type { EnvironmentId, ThreadId } from "@onicode/contracts";
import * as Option from "effect/Option";
import { useGT } from "gt-react-native";
import { m } from "../../i18n/messages";
import { useEffect, useRef } from "react";

import { downloadAndShareAttachment } from "../../lib/attachmentDownload";
import { assetEnvironment } from "../../state/assets";
import { appAtomRegistry } from "../../state/atom-registry";
import { projectEnvironment } from "../../state/projects";
import { usePreparedConnection } from "../../state/session";
import { useAtomQueryRunner } from "../../state/use-atom-query-runner";
import { fileChipShareSource, type FileChipTarget } from "./fileChipMenu";

export function useFileChipShare(
  environmentId: EnvironmentId,
  threadId: ThreadId,
  sourceIdentifier: string,
) {
  const gt = useGT();
  const connection = usePreparedConnection(environmentId);
  const httpBaseUrl = Option.isSome(connection) ? connection.value.httpBaseUrl : null;
  const createUrl = useAtomQueryRunner(assetEnvironment.createUrl, {
    refresh: true,
    reportFailure: false,
  });
  const requestRef = useRef<AbortController | null>(null);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- aborts the in-flight file share download when the component unmounts
  useEffect(() => () => requestRef.current?.abort(), []);

  return (target: FileChipTarget) => {
    const source = fileChipShareSource(target, threadId);
    if (!source || requestRef.current) return;
    const request = new AbortController();
    requestRef.current = request;
    void (async () => {
      if (httpBaseUrl === null) throw new Error(gt("Reconnect to the environment and try again."));
      // Most folder links do not end in a separator, so only the host can tell one from a file.
      if (target.fullPath) {
        const metadata = await executeAtomQuery(
          appAtomRegistry,
          projectEnvironment.fileMetadata({ environmentId, input: { path: target.fullPath } }),
          { signal: request.signal, reportFailure: false, reportDefect: false },
        );
        if (request.signal.aborted) return;
        if (metadata._tag === "Success" && metadata.value?.kind === "directory") {
          throw new Error(gt("Only files can be saved or shared, not folders."));
        }
      }
      const result = await createUrl(
        { environmentId, input: { resource: source.resource } },
        { signal: request.signal },
      );
      if (request.signal.aborted) return;
      const url =
        result._tag === "Success" ? resolveAssetUrl(httpBaseUrl, result.value.relativeUrl) : null;
      if (url === null)
        throw new Error(gt("The file could not be loaded. Reconnect and try again."));
      await downloadAndShareAttachment({
        url,
        attachment: source,
        signal: request.signal,
        sourceIdentifier,
      });
    })()
      .catch((error: unknown) => {
        if (!request.signal.aborted) {
          Dialog.alert(
            gt("Could not share file"),
            errorDisplayMessage(error, m) ?? gt("Try again."),
          );
        }
      })
      .finally(() => {
        if (requestRef.current === request) {
          requestRef.current = null;
        }
      });
  };
}
