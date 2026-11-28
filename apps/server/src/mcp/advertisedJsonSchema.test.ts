// @effect-diagnostics nodeBuiltinImport:off -- Lists toolkit folders on disk so a new toolkit is covered.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { expect, it } from "@effect/vitest";
import * as Predicate from "effect/Predicate";
import * as Schema from "effect/Schema";
import * as SchemaAST from "effect/SchemaAST";
import { Tool } from "effect/ai";

import { advertisedJsonSchema } from "./advertisedJsonSchema.ts";
import { OrchestratorToolkit } from "./toolkits/orchestrator/tools.ts";
import { ProjectToolkit } from "./toolkits/project/tools.ts";
import { ThreadToolkit } from "./toolkits/thread/tools.ts";

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

// Gemini rejects the whole turn's function declarations when any one breaks these rules.
it("advertises schemas Gemini accepts: string enums, and items on every array", async () => {
  const problems: Array<string> = [];
  const collect = (node: unknown, path: string): void => {
    if (Array.isArray(node))
      return node.forEach((member, index) => collect(member, `${path}[${index}]`));
    if (!Predicate.isObject(node)) return;
    if (node.type === "array" && !("items" in node)) problems.push(`${path}: array without items`);
    for (const [key, value] of Object.entries(node)) {
      if (
        key === "enum" &&
        Array.isArray(value) &&
        value.some((item) => typeof item !== "string")
      ) {
        problems.push(`${path}: non-string enum`);
      } else collect(value, `${path}.${key}`);
    }
  };
  for (const tool of await registeredTools()) collect(advertisedJsonSchema(tool), tool.name);

  expect(problems).toEqual([]);
});

const ownDescription = (ast: SchemaAST.AST): string | undefined =>
  SchemaAST.resolveDescription(SchemaAST.toEncoded(ast)) ?? SchemaAST.resolveDescription(ast);

// Every union member counts: a tagged union merged into one object can drop a member's own text.
const declaredDescriptions = (ast: SchemaAST.AST, path: string, found: Array<string>): void => {
  const description = ownDescription(ast);
  if (description !== undefined) found.push(`${path}: ${description}`);
  if (SchemaAST.isObjects(ast)) {
    for (const property of ast.propertySignatures) {
      declaredDescriptions(property.type, `${path}.${String(property.name)}`, found);
    }
  } else if (SchemaAST.isUnion(ast)) {
    for (const member of ast.types) declaredDescriptions(member, path, found);
  } else if (SchemaAST.isArrays(ast)) {
    for (const item of [...ast.elements, ...ast.rest]) {
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

// Every toolkit the MCP server registers lives in toolkits/<name>/tools.ts, so a new one is covered.
const registeredTools = async () => {
  const root = NodePath.join(import.meta.dirname, "toolkits");
  const tools = new Map<string, Tool.Any>();
  for (const entry of NodeFS.readdirSync(root, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const exports: Record<string, unknown> = await import(
      NodePath.join(root, entry.name, "tools.ts")
    );
    for (const value of Object.values(exports)) {
      const found =
        Predicate.hasProperty(value, "tools") && Predicate.isObject(value.tools)
          ? Object.values(value.tools)
          : [value];
      for (const tool of found) if (Tool.isUserDefined(tool)) tools.set(tool.name, tool);
    }
  }
  return [...tools.values()];
};

// Declared but never advertised, on purpose.
const unadvertised = new Set([
  // advertisedJsonSchema drops JSON-text fallbacks, so agents send the object form.
  "schedule_task.schedule: JSON-text schedule. Prefer the object form.",
  // The property's own "Retry key." replaces the shared id's longer description.
  "request_secret.clientRequestId: Reuse it to retry the same call. A new call gets a new key.",
]);

// A description on a transformed or branded schema (TrimmedNonEmptyString, ids) lands on its
// decoded side, which the JSON Schema never shows. Annotate those with Schema.annotateEncoded.
it("advertises every parameter description the tool schemas declare", async () => {
  const declared: Array<string> = [];
  const advertised = new Set<string>();
  for (const tool of await registeredTools()) {
    declaredDescriptions(tool.parametersSchema.ast, tool.name, declared);
    advertisedDescriptions(advertisedJsonSchema(tool), tool.name, advertised);
  }

  expect(declared.length).toBeGreaterThan(100);
  expect(declared.filter((entry) => !advertised.has(entry) && !unadvertised.has(entry))).toEqual(
    [],
  );
  // An entry whose description changed, went away, or is now advertised would excuse nothing.
  expect(
    [...unadvertised].filter((entry) => !declared.includes(entry) || advertised.has(entry)),
  ).toEqual([]);
});

const advertisedOptions = (
  tool: Parameters<typeof advertisedJsonSchema>[0],
  path: ReadonlyArray<string>,
) => {
  let node: unknown = advertisedJsonSchema(tool);
  for (const key of path) {
    node =
      typeof node === "object" && node !== null && "properties" in node
        ? (node as { properties?: Record<string, unknown> }).properties?.[key]
        : undefined;
  }
  return node;
};

const optionObjectMap = {
  type: "object",
  additionalProperties: { anyOf: [{ type: "string" }, { type: "boolean" }] },
};

it("advertises target.options and modelSelection.options as an object map, not an array-or-object union", () => {
  const delegate = advertisedOptions(OrchestratorToolkit.tools.delegate_task, [
    "target",
    "options",
  ]);
  expect(delegate).toEqual({
    description:
      "Ids vary by provider; see `orchestrator_capabilities`. Given values override inherited. [] or {} clears.",
    ...optionObjectMap,
  });
  expect(delegate).not.toHaveProperty("anyOf");

  for (const tool of [
    ProjectToolkit.tools.oni_thread_launch,
    ThreadToolkit.tools.oni_thread_configure,
  ]) {
    const options = advertisedOptions(tool, ["modelSelection", "options"]);
    expect(options, tool.name).toMatchObject({ type: "object" });
    expect(options, tool.name).not.toHaveProperty("anyOf");
    expect(options, tool.name).not.toHaveProperty("items");
  }
});
