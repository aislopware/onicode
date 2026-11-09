import * as Exit from "effect/Exit";
import * as Schema from "effect/Schema";

import { OniProjectFile, ONICODE_PROJECT_FILE_SCHEMA_URL } from "@onicode/contracts";

import { fromLenientJson } from "./schemaJson.ts";

export const OniProjectFileFromJson = fromLenientJson(OniProjectFile);

const decodeOniProjectFile = Schema.decodeExit(OniProjectFileFromJson);

export function parseOniProjectFile(contents: string): OniProjectFile | null {
  const decoded = decodeOniProjectFile(contents);
  return Exit.isSuccess(decoded) ? decoded.value : null;
}

export function buildOniProjectFileJsonSchema(): Record<string, unknown> {
  const document = Schema.toJsonSchemaDocument(OniProjectFile, { onExcessProperty: "error" });
  const jsonSchema: Record<string, unknown> = {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    $id: ONICODE_PROJECT_FILE_SCHEMA_URL,
    ...document.schema,
  };
  if (document.definitions && Object.keys(document.definitions).length > 0) {
    jsonSchema.$defs = document.definitions;
  }
  return jsonSchema;
}
