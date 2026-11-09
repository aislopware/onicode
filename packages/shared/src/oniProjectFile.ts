import * as Exit from "effect/Exit";
import * as Schema from "effect/Schema";

import { OniProjectFile } from "@onicode/contracts";

import { fromLenientJson } from "./schemaJson.ts";

export const OniProjectFileFromJson = fromLenientJson(OniProjectFile);

const decodeOniProjectFile = Schema.decodeExit(OniProjectFileFromJson);

export function parseOniProjectFile(contents: string): OniProjectFile | null {
  const decoded = decodeOniProjectFile(contents);
  return Exit.isSuccess(decoded) ? decoded.value : null;
}
