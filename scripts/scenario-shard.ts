// @effect-diagnostics nodeBuiltinImport:off globalConsole:off - a CI helper that prints file names for the shell.
// Prints the scenario files of one shard: `node scripts/scenario-shard.ts 2/4`.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { shardScenarios } from "./lib/scenario-shards.ts";

const directory = NodePath.join(import.meta.dirname, "scenarios");
const files = NodeFS.readdirSync(directory)
  .filter((name) => name.endsWith(".e2e.ts"))
  .map((name) => `scenarios/${name}`);
const weights: Record<string, number> = JSON.parse(
  NodeFS.readFileSync(NodePath.join(directory, "shard-weights.json"), "utf8"),
);
console.log(shardScenarios(files, weights, process.argv[2] ?? "").join(" "));
