// @effect-diagnostics nodeBuiltinImport:off - test support that reads highlights.wasm from disk.
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";

import { init } from "@pierre/highlights";

// The highlighter's line-record API, which `Highlighter` does not declare.
interface LineRecordHighlighter {
  writeInput(input: string): number;
  tokenizeLineRecords(language: number, byteLength: number): Uint32Array;
}

/**
 * Stands in for the OniHighlights hybrid object with the WebAssembly build it is
 * compiled from. Return it from a mocked `NitroModules.createHybridObject`.
 */
export function createWasmHighlightsModule() {
  const require = NodeModule.createRequire(import.meta.url);
  const wasm = new WebAssembly.Module(
    NodeFS.readFileSync(require.resolve("@pierre/highlights/highlights.wasm")),
  );
  const highlighter = init(wasm) as unknown as LineRecordHighlighter;
  return {
    tokenize(source: string, language: number): ArrayBuffer {
      const records = highlighter.tokenizeLineRecords(language, highlighter.writeInput(source));
      // The view points into WebAssembly memory that the next call reuses.
      return records.slice().buffer;
    },
  };
}
