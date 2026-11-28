import { FilePreviewDecodeError } from "@onicode/shared/filePreview";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { AsyncResult } from "effect/reactivity";

const isFilePreviewDecodeError = Schema.is(FilePreviewDecodeError);

/**
 * Why a file the server would not read as text has no preview. The viewer needs the same URL,
 * so a failed mint hides it; bytes that are not text stay that way, so they get no retry.
 */
export function binaryFileFailure(
  downloadUrl: AsyncResult.AsyncResult<unknown, unknown>,
  download: AsyncResult.AsyncResult<unknown, unknown>,
) {
  if (AsyncResult.isFailure(downloadUrl)) {
    return {
      cause: Option.getOrNull(AsyncResult.error(downloadUrl)),
      canOpenInViewer: false,
      canRetry: true,
    };
  }
  if (!AsyncResult.isFailure(download)) return null;
  const cause = Option.getOrNull(AsyncResult.error(download));
  return {
    cause,
    canOpenInViewer: true,
    canRetry: !isFilePreviewDecodeError(cause),
  };
}
