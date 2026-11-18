#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off globalConsole:off
/**
 * Saves what an obu binary tells an MCP client, for `browser-driver.json`. Usage:
 * `pnpm mcp-tool-use:capture-browser-driver <path-to-obu>`, with the release Onicode pins.
 */
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

type Reply = {
  readonly result?: {
    readonly serverInfo?: { readonly version: string };
    readonly instructions?: string;
    readonly tools?: ReadonlyArray<{
      readonly name: string;
      readonly description: string;
      readonly inputSchema: unknown;
    }>;
  };
};

const [binary] = process.argv.slice(2);
if (binary === undefined) {
  console.error("usage: capture-browser-driver.ts <path-to-obu>");
  process.exit(1);
}
const requests = [
  {
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "onicode-evals", version: "0" },
    },
  },
  { jsonrpc: "2.0", method: "notifications/initialized" },
  { jsonrpc: "2.0", id: 2, method: "tools/list" },
];
// A home of its own keeps the capture away from a driver that is in use.
const home = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "obu-capture-"));
const stdout = NodeChildProcess.execFileSync(binary, ["mcp"], {
  input: `${requests.map((request) => JSON.stringify(request)).join("\n")}\n`,
  encoding: "utf8",
  env: { ...process.env, OBU_HOME: home, OBU_HOST_NOTE: "{{HOST_NOTE}}" },
});
NodeFS.rmSync(home, { recursive: true, force: true });
const replies = stdout
  .split("\n")
  .filter((line) => line.trim() !== "")
  .map((line) => (JSON.parse(line) as Reply).result ?? {});
const hello = replies.find((reply) => reply.instructions !== undefined);
const listed = replies.find((reply) => reply.tools !== undefined);
if (hello?.serverInfo === undefined || listed?.tools === undefined) {
  console.error("The binary answered neither initialize nor tools/list.");
  process.exit(1);
}
const capture = {
  version: hello.serverInfo.version,
  instructions: hello.instructions,
  tools: listed.tools.map((tool) => ({
    name: tool.name,
    description: tool.description,
    input_schema: tool.inputSchema,
  })),
};
NodeFS.writeFileSync(
  NodePath.join(import.meta.dirname, "browser-driver.json"),
  `${JSON.stringify(capture, null, 2)}\n`,
);
console.log(`Saved obu ${capture.version}: ${capture.tools.length} tools.`);
