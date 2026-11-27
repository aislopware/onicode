import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { expect, it } from "vite-plus/test";

import { HIGHLIGHTS_LANGUAGE_IDS } from "../../../src/lib/highlights.generated";

const MODE_UTF16_LINE_RECORDS = 3;

// Runs highlights.wasm the way the JavaScript glue does, as the reference for the C host's
// buffer layout, which a @pierre/highlights release can move (inputPtr did in 0.6.0).
const wasmRecords = (source: Uint8Array, language: number): ReadonlyArray<number> => {
  const require = NodeModule.createRequire(import.meta.url);
  const module = new WebAssembly.Module(
    NodeFS.readFileSync(require.resolve("@pierre/highlights/highlights.wasm")),
  );
  const imports = Object.fromEntries(
    WebAssembly.Module.imports(module).map(({ name }) => [name, () => 0]),
  );
  const exports = new WebAssembly.Instance(module, { env: imports }).exports as {
    readonly memory: WebAssembly.Memory;
    readonly inputPtr: WebAssembly.Global;
    readonly highlight: () => void;
  };
  const inputPtr = Number(exports.inputPtr.value);
  const needed = inputPtr + source.length + 96 - exports.memory.buffer.byteLength;
  if (needed > 0) exports.memory.grow(Math.ceil(needed / 65_536));
  let view = new DataView(exports.memory.buffer);
  new Uint8Array(exports.memory.buffer).set(source, inputPtr);
  view.setUint8(inputPtr + source.length, 0);
  view.setUint8(0, language);
  view.setUint8(1, MODE_UTF16_LINE_RECORDS);
  view.setUint32(2, source.length, true);
  exports.highlight();
  view = new DataView(exports.memory.buffer);
  const start = view.getUint32(6, true);
  return Array.from(new Uint32Array(exports.memory.buffer, start, view.getUint32(10, true) >> 2));
};

it("tokenizes like highlights.wasm", () => {
  const moduleDir = NodePath.resolve(import.meta.dirname, "..");
  const output = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "oni-highlights-"));
  try {
    const binary = NodePath.join(output, "tokenize");
    NodeChildProcess.execFileSync("cc", [
      "-O0",
      "-DWASM_RT_USE_MMAP=0",
      "-DWASM_RT_MEMCHECK_BOUNDS_CHECK=1",
      "-DWASM_RT_SKIP_SIGNAL_RECOVERY=1",
      ...["cpp", "Vendor", "Vendor/highlights", "Vendor/wasm2c"].map(
        (dir) => `-I${NodePath.join(moduleDir, dir)}`,
      ),
      ...[
        "tests/tokenize.c",
        "cpp/oni_highlights.c",
        "Vendor/highlights/highlights.c",
        "Vendor/wasm2c/wasm-rt-impl.c",
        "Vendor/wasm2c/wasm-rt-mem-impl.c",
      ].map((file) => NodePath.join(moduleDir, file)),
      "-lm",
      "-o",
      binary,
    ]);
    const sourcePath = NodePath.join(moduleDir, "cpp/oni_highlights.c");
    const source = NodeFS.readFileSync(sourcePath);
    for (const language of ["c", "markdown"] as const) {
      const id = HIGHLIGHTS_LANGUAGE_IDS[language];
      if (id === undefined) throw new Error(`no lexer for ${language}`);
      const native = NodeChildProcess.execFileSync(binary, [sourcePath, String(id)], {
        encoding: "utf8",
      })
        .trim()
        .split("\n")
        .map(Number);
      expect(native.length).toBeGreaterThan(0);
      expect(native).toEqual(wasmRecords(source, id));
    }
  } finally {
    NodeFS.rmSync(output, { recursive: true, force: true });
  }
});
