// @effect-diagnostics nodeBuiltinImport:off
import * as NodeModule from "node:module";
import { parseSync } from "vite-plus";

const LITERAL_SPECIFIER = /^(["'`])([^$`\\]*)\1$/;

/** Node single-executable imports resolve only builtins; use createRequire for file-backed external packages. */
export function findEsmImportsOfExternalPackages(source: string): ReadonlyArray<string> {
  const specifiers = new Set<string>();
  const { module, errors } = parseSync("bundle.mjs", source, { sourceType: "module", lang: "js" });
  const [error] = errors;
  if (error) throw new Error(`Could not parse the CLI bundle: ${error.message}`);
  const add = (specifier: string, dynamic: boolean) => {
    // Optional Bun imports are runtime builtins; static Bun imports still fail under Node.
    const runtimeBuiltin =
      NodeModule.isBuiltin(specifier) || (dynamic && specifier.startsWith("bun:"));
    if (!runtimeBuiltin && !specifier.startsWith("./") && !specifier.startsWith("../")) {
      specifiers.add(specifier);
    }
  };
  for (const { moduleRequest } of module.staticImports) add(moduleRequest.value, false);
  for (const { entries } of module.staticExports) {
    for (const { moduleRequest } of entries) {
      if (moduleRequest) add(moduleRequest.value, false);
    }
  }
  for (const { moduleRequest } of module.dynamicImports) {
    const literal = LITERAL_SPECIFIER.exec(source.slice(moduleRequest.start, moduleRequest.end));
    if (literal?.[2] !== undefined) add(literal[2], true);
  }
  return [...specifiers].sort();
}
