import type { AssetResource, EnvironmentId, ThreadId } from "@onicode/contracts";
import { useAtomValue } from "@effect/atom-react";
import * as Effect from "effect/Effect";
import { FetchHttpClient, HttpClient, HttpClientResponse } from "effect/http";
import { AsyncResult, Atom } from "effect/reactivity";
import { useMemo } from "react";

import { connectionAtomRuntime } from "../../connection/runtime";
import { useAssetUrlState } from "../../state/assets";

// Whole-file reads past this stay on the 1 MB RPC preview; a phone lays out every source row.
export const MOBILE_FULL_FILE_MAX_BYTES = 8 * 1024 * 1024;

/**
 * Workspace-file URLs serve only previewable types; an attachment download serves any file. A
 * draft has no thread yet, so its download is scoped to the draft's workspace instead.
 */
export function fullFileTextResource(input: {
  readonly threadId: ThreadId | null;
  readonly draftCwd: string | null;
  readonly absolutePath: string | null;
}): AssetResource | null {
  const { threadId, draftCwd, absolutePath } = input;
  if (absolutePath === null) return null;
  if (threadId !== null)
    return { _tag: "media-file", threadId, path: absolutePath, disposition: "attachment" };
  if (draftCwd === null) return null;
  return {
    _tag: "draft-workspace-file",
    cwd: draftCwd,
    path: absolutePath,
    disposition: "attachment",
  };
}

/** Null for what the RPC preview rejects as binary: a NUL byte or invalid UTF-8. */
export function decodeFullFileText(bytes: Uint8Array): string | null {
  if (bytes.includes(0)) return null;
  try {
    // A leading BOM is dropped, as in the preview read.
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/**
 * A refresh can keep the URL, so the key also carries the revision it downloads for. No idle TTL:
 * the revision cannot see a same-size edit, so a reopened file downloads its text again.
 */
const fullFileTextAtom = Atom.family((key: string) => {
  const [uri] = JSON.parse(key) as [string, string];
  return connectionAtomRuntime
    .atom(
      HttpClient.get(uri).pipe(
        Effect.flatMap(HttpClientResponse.filterStatusOk),
        Effect.flatMap((response) => {
          // The file can grow after the preview read; never buffer past the cap. React Native's
          // fetch buffers the whole body, so a response without a length is refused, not trusted.
          const length = Number(response.headers["content-length"] ?? Number.NaN);
          return Number.isFinite(length) && length <= MOBILE_FULL_FILE_MAX_BYTES
            ? Effect.map(response.arrayBuffer, (buffer) =>
                decodeFullFileText(new Uint8Array(buffer)),
              )
            : Effect.succeed(null);
        }),
        Effect.provideService(FetchHttpClient.RequestInit, { cache: "no-store" }),
      ),
    )
    .pipe(Atom.withLabel(`full-file-text:${key}`));
});

const noFullFileTextAtom = Atom.make(AsyncResult.success(null));

/**
 * Downloads a thread or draft file's full text past the 1 MB RPC preview; null while loading, after
 * a failure, for binary contents, or when disabled. A new `revision` downloads it again.
 */
export function useFullFileText(input: {
  readonly environmentId: EnvironmentId | null;
  readonly threadId: ThreadId | null;
  readonly draftCwd: string | null;
  readonly absolutePath: string | null;
  readonly revision: string;
}): string | null {
  const { threadId, draftCwd, absolutePath } = input;
  const resource = useMemo(
    () => fullFileTextResource({ threadId, draftCwd, absolutePath }),
    [threadId, draftCwd, absolutePath],
  );
  const assetUrl = useAssetUrlState(input.environmentId, resource, input.revision);
  const uri = resource !== null && assetUrl._tag === "Success" ? assetUrl.url : null;
  const result = useAtomValue(
    uri === null ? noFullFileTextAtom : fullFileTextAtom(JSON.stringify([uri, input.revision])),
  );
  // The caller keeps showing the 1 MB preview and its partial-file notice.
  return AsyncResult.isSuccess(result) ? result.value : null;
}
