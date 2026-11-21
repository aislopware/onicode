import { expect, it } from "@effect/vitest";
import * as Predicate from "effect/Predicate";
import * as Schema from "effect/Schema";
import * as SchemaAST from "effect/SchemaAST";
import { Tool } from "effect/ai";

import { advertisedJsonSchema } from "./advertisedJsonSchema.ts";
import { AttachmentToolkit } from "./toolkits/attachment/tools.ts";
import { DeviceToolkit } from "./toolkits/device/tools.ts";
import { EnvironmentToolkit } from "./toolkits/environment/tools.ts";
import { HtmlToolkit } from "./toolkits/html/tools.ts";
import { IssuesToolkit } from "./toolkits/issues/tools.ts";
import { MessageToolkit } from "./toolkits/message/tools.ts";
import { OrchestratorToolkit } from "./toolkits/orchestrator/tools.ts";
import { PreviewToolkit } from "./toolkits/preview/tools.ts";
import { PreviewControlsToolkit } from "./toolkits/previewControls/tools.ts";
import { ProjectToolkit } from "./toolkits/project/tools.ts";
import { PullRequestsToolkit } from "./toolkits/pullRequests/tools.ts";
import { ThemeToolkit } from "./toolkits/theme/tools.ts";
import { ThreadToolkit } from "./toolkits/thread/tools.ts";
import { ToolSearchToolkit } from "./toolkits/toolSearch/tools.ts";
import { WorktreeToolkit } from "./toolkits/worktree/tools.ts";

it("drops JSON noise, merges tagged unions, and keeps null where it is required", () => {
  const Every = Schema.Struct({ ms: Schema.Int });
  const tool = Tool.make("example", {
    parameters: Schema.Struct({
      threadId: Schema.optional(Schema.NullOr(Schema.String)),
      beforeRunId: Schema.NullOr(Schema.String),
      limit: Schema.optional(Schema.Int),
      every: Schema.Union([Every, Schema.fromJsonString(Every)]),
      target: Schema.optional(
        Schema.NullOr(
          Schema.Union([
            Schema.Struct({ kind: Schema.Literal("url"), url: Schema.String }),
            Schema.Struct({ kind: Schema.Literal("number"), number: Schema.Int }),
          ]),
        ),
      ),
    }),
  });

  expect(advertisedJsonSchema(tool)).toEqual({
    type: "object",
    properties: {
      threadId: { type: "string" },
      beforeRunId: { anyOf: [{ type: "string" }, { type: "null" }] },
      limit: { type: "integer" },
      every: { type: "object", properties: { ms: { type: "integer" } }, required: ["ms"] },
      target: {
        type: "object",
        properties: {
          kind: { type: "string", enum: ["url", "number"] },
          url: { type: "string" },
          number: { type: "integer" },
        },
        required: ["kind"],
      },
    },
    required: ["beforeRunId", "every"],
  });
});

it("advertises only string enum values, which Gemini requires", () => {
  const enumValues: Array<unknown> = [];
  const collect = (node: unknown): void => {
    if (Array.isArray(node)) return node.forEach(collect);
    if (typeof node !== "object" || node === null) return;
    for (const [key, value] of Object.entries(node)) {
      if (key === "enum" && Array.isArray(value)) enumValues.push(...value);
      else collect(value);
    }
  };
  collect(advertisedJsonSchema(ThemeToolkit.tools.oni_theme_set));

  expect(enumValues.filter((value) => typeof value !== "string")).toEqual([]);
});

const declaredDescription = (ast: SchemaAST.AST): string | undefined =>
  SchemaAST.resolveDescription(SchemaAST.toEncoded(ast)) ??
  SchemaAST.resolveDescription(ast) ??
  (SchemaAST.isUnion(ast)
    ? ast.types.map(declaredDescription).find((description) => description !== undefined)
    : undefined);

const declaredDescriptions = (ast: SchemaAST.AST, path: string, found: Array<string>): void => {
  if (SchemaAST.isObjects(ast)) {
    for (const property of ast.propertySignatures) {
      const at = `${path}.${String(property.name)}`;
      const description = declaredDescription(property.type);
      if (description !== undefined) found.push(`${at}: ${description}`);
      declaredDescriptions(property.type, at, found);
    }
  } else if (SchemaAST.isUnion(ast)) {
    for (const member of ast.types) declaredDescriptions(member, path, found);
  } else if (SchemaAST.isArrays(ast)) {
    for (const item of [...ast.elements, ...ast.rest]) {
      const description = declaredDescription(item);
      if (description !== undefined) found.push(`${path}[]: ${description}`);
      declaredDescriptions(item, `${path}[]`, found);
    }
  }
};

const advertisedDescriptions = (node: unknown, path: string, found: Set<string>): void => {
  if (Array.isArray(node))
    return node.forEach((member) => advertisedDescriptions(member, path, found));
  if (!Predicate.isObject(node)) return;
  if (typeof node.description === "string") found.add(`${path}: ${node.description}`);
  if (Predicate.isObject(node.properties)) {
    for (const [name, property] of Object.entries(node.properties)) {
      advertisedDescriptions(property, `${path}.${name}`, found);
    }
  }
  for (const members of [node.anyOf, node.oneOf, node.allOf]) {
    advertisedDescriptions(members, path, found);
  }
  for (const items of [node.items, node.prefixItems])
    advertisedDescriptions(items, `${path}[]`, found);
};

// A description on a transformed or branded schema (TrimmedNonEmptyString, ids) lands on its
// decoded side, which the JSON Schema never shows. Annotate those with Schema.annotateEncoded.
it("advertises every parameter description the tool schemas declare", () => {
  const toolkits = [
    AttachmentToolkit,
    DeviceToolkit,
    EnvironmentToolkit,
    HtmlToolkit,
    IssuesToolkit,
    MessageToolkit,
    OrchestratorToolkit,
    PreviewToolkit,
    PreviewControlsToolkit,
    ProjectToolkit,
    PullRequestsToolkit,
    ThemeToolkit,
    ThreadToolkit,
    ToolSearchToolkit,
    WorktreeToolkit,
  ];
  const declared: Array<string> = [];
  const advertised = new Set<string>();
  for (const tool of toolkits.flatMap((toolkit) => Object.values<Tool.Any>(toolkit.tools))) {
    declaredDescriptions(tool.parametersSchema.ast, tool.name, declared);
    advertisedDescriptions(advertisedJsonSchema(tool), tool.name, advertised);
  }

  expect(declared.length).toBeGreaterThan(100);
  expect(declared.filter((entry) => !advertised.has(entry))).toEqual([]);
});
