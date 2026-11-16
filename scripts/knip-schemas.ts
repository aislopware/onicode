// @effect-diagnostics nodeBuiltinImport:off - Knip preprocessors resolve paths synchronously.
import * as NodePath from "node:path";
import type { Preprocessor } from "knip";
// TypeScript 7 exposes its checker only through this API, which drives the native compiler.
import { API, isStringLiteralType, SymbolFlags } from "typescript-native/unstable/sync";

const schemaTypeId = "~effect/Schema/Schema";

// Drops unused-export issues whose every symbol is an Effect schema.
const preprocess: Preprocessor = (options) => {
  const categories = ["exports", "nsExports", "duplicates"] as const;
  // Only the names the issues mention are checked: inferring the type of every export costs seconds.
  const files = new Map<string, Set<string>>();
  for (const category of categories) {
    for (const [filePath, issues] of Object.entries(options.issues[category])) {
      if (Object.keys(issues).length === 0) continue;
      const names = files.get(filePath) ?? new Set<string>();
      for (const issue of Object.values(issues)) {
        for (const { symbol } of issue.symbols ?? [{ symbol: issue.symbol }]) names.add(symbol);
      }
      files.set(filePath, names);
    }
  }
  if (files.size === 0) return options;

  const api = new API();
  try {
    // Each open file loads the tsconfig project that contains it, or an inferred one.
    const snapshot = api.updateSnapshot({
      openFiles: [...files.keys()].map((filePath) => NodePath.resolve(options.cwd, filePath)),
    });
    for (const [filePath, names] of files) {
      const file = NodePath.resolve(options.cwd, filePath);
      const project = snapshot.getDefaultProjectForFile(file);
      if (!project) continue;
      const { checker } = project;
      const source = project.program.getSourceFile(file);
      const module = source && checker.getSymbolAtLocation(source);
      if (!source || !module) continue;
      const schemas = new Set(
        checker
          .getExportsOfModule(module)
          .filter((symbol) => names.has(symbol.name))
          .filter((symbol) => {
            const exported =
              symbol.flags & SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
            const target =
              exported.flags & SymbolFlags.Value
                ? exported
                : checker.resolveName(symbol.name, SymbolFlags.Value, source, false);
            const type = target && checker.getTypeOfSymbol(target);
            const marker = type && checker.getPropertyOfType(type, schemaTypeId);
            const markerType = marker && checker.getTypeOfSymbol(marker);
            return (
              markerType !== undefined &&
              isStringLiteralType(markerType) &&
              markerType.value === schemaTypeId
            );
          })
          .map((symbol) => symbol.name),
      );
      for (const category of categories) {
        const issues = options.issues[category][filePath];
        if (!issues) continue;
        for (const [key, issue] of Object.entries(issues)) {
          const symbols = issue.symbols ?? [{ symbol: issue.symbol }];
          if (symbols.length > 0 && symbols.every(({ symbol }) => schemas.has(symbol))) {
            delete issues[key];
            options.counters[category]--;
          }
        }
        if (Object.keys(issues).length === 0) delete options.issues[category][filePath];
      }
    }
  } finally {
    api.close();
  }
  return options;
};

export default preprocess;
