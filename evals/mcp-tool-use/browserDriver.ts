// The eval reads the driver's captured MCP surface from a file beside it.
// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as Schema from "effect/Schema";

import { ISOLATED_NOTE, PANEL_NOTE } from "../../apps/server/src/browserUse/BrowserUseService.ts";
import { PANEL_URL_REQUIRED } from "../../apps/server/src/preview/previewCdpPolicy.ts";

/** The browser a thread's obu drives: its tab in the Browser panel, or a Chrome of its own. */
export const BrowserDriverTarget = Schema.Literals(["panel", "isolated"]);
export type BrowserDriverTarget = typeof BrowserDriverTarget.Type;

export const BROWSER_DRIVER_PREFIX = "mcp__browser__";
const HOST_NOTE = "{{HOST_NOTE}}";

/**
 * obu's `initialize` instructions and `tools/list`, captured from the release Onicode pins with
 * `OBU_HOST_NOTE={{HOST_NOTE}}`. obu is a binary, so its text cannot be imported from source.
 */
export const browserDriverCapture = Schema.decodeSync(
  Schema.fromJsonString(
    Schema.Struct({
      version: Schema.String,
      instructions: Schema.String,
      tools: Schema.Array(
        Schema.Struct({
          name: Schema.String,
          description: Schema.String,
          input_schema: Schema.Unknown,
        }),
      ),
    }),
  ),
)(NodeFS.readFileSync(NodePath.join(import.meta.dirname, "browser-driver.json"), "utf8"));

const hostNote = (target: BrowserDriverTarget) => (target === "panel" ? PANEL_NOTE : ISOLATED_NOTE);

export const browserDriverTools = browserDriverCapture.tools.map((tool) => ({
  ...tool,
  name: `${BROWSER_DRIVER_PREFIX}${tool.name}`,
}));

export const isBrowserDriverTool = (name: string) => name.startsWith(BROWSER_DRIVER_PREFIX);

const isRecord = (value: unknown): value is Readonly<Record<string, unknown>> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const schemaOf = (tool: string) => {
  const schema = browserDriverCapture.tools.find((entry) => entry.name === tool)?.input_schema;
  return isRecord(schema) && isRecord(schema.properties) ? schema.properties : {};
};
// The captured release decides what `vision` takes, so a case fails on a driver that lacks it.
const visionFields = new Set(Object.keys(schemaOf("vision")));
const runAction = schemaOf("run").steps;
const visionIsRunStep = JSON.stringify(runAction ?? null).includes('"vision"');

/** The block Claude Code builds from an MCP server's instructions. */
export const browserDriverInstructions = (target: BrowserDriverTarget) =>
  [
    "# MCP Server Instructions",
    "The following MCP servers have provided instructions for how to use their tools and resources:",
    `## browser\n${browserDriverCapture.instructions.replace(HOST_NOTE, hostNote(target))}`,
  ].join("\n\n");

/** What a sample's driver has done so far. */
export interface BrowserDriverState {
  url: string | undefined;
  /** obu says whose browser it drives on its first result too. */
  noted: boolean;
  /** The screenshots `vision` wrote, each as the shell simulator should find it. */
  readonly saved: Array<string>;
}

export interface BrowserDriverResult {
  readonly text: string;
  readonly isError: boolean;
}

/**
 * obu's result for a call the case does not mock. It keeps the facts an agent's next step turns
 * on: the panel refuses a local file, a named file gets the capture, a page has the world's text.
 */
export const browserDriverResult = (
  state: BrowserDriverState,
  target: BrowserDriverTarget,
  tool: string,
  input: Readonly<Record<string, unknown>>,
  world: { readonly cwd: string; readonly pages?: Readonly<Record<string, string>> | undefined },
): BrowserDriverResult => {
  const failed = (text: string): BrowserDriverResult => ({
    text: `✗ error: ${text}`,
    isError: true,
  });
  const pageText = (url: string) => {
    const html = [url, url.replace(/\/$/, ""), `${url}/`]
      .map((key) => world.pages?.[key])
      .find((page) => page !== undefined);
    return html
      ?.replace(/<[^>]+>/g, " ")
      .replace(/\s+/g, " ")
      .trim();
  };
  const navigate = (url: string) => {
    if (target === "panel" && !/^https?:\/\//i.test(url) && /^[a-z][a-z0-9+.-]*:/i.test(url)) {
      return failed(PANEL_URL_REQUIRED);
    }
    state.url = /^[a-z][a-z0-9+.-]*:/i.test(url) ? url : `https://${url}`;
    return undefined;
  };
  const page = () =>
    `${state.url ?? "about:blank"}\n${(state.url && pageText(state.url)) ?? "(page loaded)"}`;
  const shot = (args: Readonly<Record<string, unknown>>) => {
    const step = Object.fromEntries(Object.entries(args).filter(([key]) => visionFields.has(key)));
    const size = /^(\d+)x(\d+)(?:@([\d.]+))?$/i.exec(
      typeof step.size === "string" ? step.size : "",
    );
    if (typeof step.size === "string" && step.size !== "" && !size) {
      return failed(`vision size must be WxH or WxH@scale (e.g. 1440x900@2), got '${step.size}'`);
    }
    const scale = Number(size?.[3] ?? 1);
    const width = Number(size?.[1] ?? 1280) * scale;
    const height = (step.full === true ? 4200 : Number(size?.[2] ?? 800)) * scale;
    // obu does not name the page. It is named here so a case can tell which one was shot.
    const of = ` of ${state.url ?? "about:blank"}`;
    if (typeof step.path !== "string" || step.path === "") {
      return { text: `screenshot${of}`, isError: false };
    }
    if (!step.path.toLowerCase().endsWith(".png")) {
      return failed(`vision path must end in .png, got '${step.path}'`);
    }
    const file = step.path.startsWith("/") ? step.path : `${world.cwd}/${step.path}`;
    state.saved.push(`${file} (PNG image data, ${width} x ${height})`);
    return {
      text: `screenshot saved to ${file} (${width}x${height} px, ${Math.round((width * height) / 40_000)} KB)${of}`,
      isError: false,
    };
  };
  const step = (action: string, args: Readonly<Record<string, unknown>>): BrowserDriverResult => {
    const takesUrl = action !== "vision" || visionFields.has("url");
    const url = takesUrl && typeof args.url === "string" && args.url !== "" ? args.url : undefined;
    const refused = url === undefined ? undefined : navigate(url);
    if (refused) return refused;
    switch (action) {
      case "vision":
        return shot(args);
      case "see":
      case "navigate":
      case "extract":
      case "read":
        return { text: page(), isError: false };
      case "eval":
        return { text: "result: null", isError: false };
      default:
        return { text: `ok: ${action}${url === undefined ? "" : `\n${page()}`}`, isError: false };
    }
  };
  const run = (): BrowserDriverResult => {
    if (tool !== "run" && !(tool === "act" && input.action === "batch")) {
      return step(tool === "act" ? String(input.action ?? "") : tool, input);
    }
    const lines: Array<string> = [];
    for (const [index, entry] of (Array.isArray(input.steps) ? input.steps : []).entries()) {
      const args = isRecord(entry) ? entry : {};
      const action = String(args.action ?? "");
      const result =
        action === "vision" && !visionIsRunStep
          ? failed(`step ${index + 1}: unknown action 'vision'`)
          : step(action, args);
      lines.push(`step ${index + 1}: ${result.text}`);
      if (result.isError) return { text: lines.join("\n"), isError: true };
    }
    return { text: lines.join("\n") || "ok", isError: false };
  };
  const result = run();
  if (state.noted) return result;
  state.noted = true;
  return { ...result, text: `note: ${hostNote(target)}\n${result.text}` };
};
