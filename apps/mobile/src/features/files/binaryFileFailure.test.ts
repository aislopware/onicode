import { FilePreviewDecodeError } from "@onicode/shared/filePreview";
import { AsyncResult } from "effect/reactivity";
import { expect, it } from "vite-plus/test";

import { binaryFileFailure } from "./binaryFileFailure";

const offline = new Error("offline");
const notText = new FilePreviewDecodeError({ message: "Not text", i18nMessage: ["Not text"] });

it.each([
  ["a pending download", AsyncResult.initial(true), AsyncResult.initial(), null],
  ["decoded text", AsyncResult.success("url"), AsyncResult.success("text"), null],
  [
    "a URL that could not be minted",
    AsyncResult.fail(offline),
    AsyncResult.initial(),
    { cause: offline, canOpenInViewer: false, canRetry: true },
  ],
  [
    "a failed download",
    AsyncResult.success("url"),
    AsyncResult.fail(offline),
    { cause: offline, canOpenInViewer: true, canRetry: true },
  ],
  [
    "bytes that are not text",
    AsyncResult.success("url"),
    AsyncResult.fail(notText),
    { cause: notText, canOpenInViewer: true, canRetry: false },
  ],
])("offers what still works after %s", (_, url, download, expected) => {
  expect(binaryFileFailure(url, download)).toEqual(expected);
});
