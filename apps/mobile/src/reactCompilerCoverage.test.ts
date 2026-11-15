// @effect-diagnostics nodeBuiltinImport:off - compiles real source files on disk.
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import { describe, expect, it } from "vite-plus/test";

const mobileRoot = NodePath.resolve(__dirname, "..");
const requireFromMobile = NodeModule.createRequire(NodePath.join(mobileRoot, "package.json"));
const babel: {
  transformFileSync: (file: string, options: Record<string, unknown>) => unknown;
} = requireFromMobile("@babel/core");
const metroConfig: Promise<{ transformer: { babelTransformerPath: string } }> =
  requireFromMobile("./metro.config.js");

// Remove bailout entries and manual memoization when compilation starts; this check rejects stale entries.
const KNOWN_BAILOUTS: Record<string, string> = {
  "src/components/ui/Menu.tsx": "ref read during render",
  "src/features/diffs/nativeReviewDiffSurface.ts": "ref read during render",
  "src/features/home/thread-swipe-actions.tsx": "ref read during render",
  "src/features/threads/ThreadArrangementSheet.tsx": "ref read during render",
  "src/features/voice-input/useVoiceInputController.ts": "ref read during render",
  "src/native/OniComposerEditor.ios.tsx": "ref read during render",
  "src/native/OniComposerEditor.native.tsx": "ref read during render",
};

// Use Metro's iOS release flags; experiments.reactCompiler enables supportsReactCompiler.
const metroCaller = {
  name: "metro",
  bundler: "metro",
  platform: "ios",
  isDev: false,
  isServer: false,
  isNodeModule: false,
  supportsStaticESM: true,
  supportsReactCompiler: true,
};

const sourceFiles = NodeFS.readdirSync(NodePath.join(mobileRoot, "src"), { recursive: true })
  .map(String)
  .filter((file) => /\.tsx?$/.test(file) && !/\.(test\.tsx?|d\.ts)$/.test(file))
  .map((file) => `src/${file.split(NodePath.sep).join("/")}`)
  .sort();

type CompilerLoc = { readonly start?: { readonly line?: number } } | null;

interface CompilerEvent {
  readonly kind: string;
  readonly detail?: {
    readonly options?: {
      readonly category?: string;
      readonly reason?: string;
      readonly loc?: CompilerLoc;
      readonly details?: readonly { readonly loc?: CompilerLoc }[];
    };
  };
}

// babel.config.js is the file Metro loads; the logger rides on the preset's
// own react-compiler option so the plugin runs with Metro's exact settings.
function compileLikeMetro(file: string, events: CompilerEvent[], output: Record<string, unknown>) {
  const babelConfig = requireFromMobile("./babel.config.js")({ cache: () => {} });
  const presets = babelConfig.presets.map(([name, options]: [string, object]) =>
    name === "babel-preset-expo"
      ? [
          requireFromMobile.resolve(name),
          {
            ...options,
            "react-compiler": {
              logger: { logEvent: (_: string, event: CompilerEvent) => events.push(event) },
            },
          },
        ]
      : [name, options],
  );
  return babel.transformFileSync(NodePath.join(mobileRoot, file), {
    babelrc: false,
    configFile: false,
    cwd: mobileRoot,
    presets,
    caller: metroCaller,
    code: false,
    ...output,
  });
}

function compilerBailouts(file: string): string[] {
  const events: CompilerEvent[] = [];
  compileLikeMetro(file, events, {});
  return events
    .filter((event) => event.kind === "CompileError" || event.kind === "CompileSkip")
    .map(
      (event) =>
        `line ${(event.detail?.options?.loc ?? event.detail?.options?.details?.[0]?.loc)?.start?.line ?? "?"}: ${event.detail?.options?.category ?? event.kind} ${event.detail?.options?.reason ?? ""}`,
    );
}

interface AstNode {
  readonly type: string;
  readonly loc?: CompilerLoc;
  readonly [key: string]: unknown;
}

const isAstNode = (value: unknown): value is AstNode =>
  typeof value === "object" && value !== null && "type" in value && typeof value.type === "string";

function walkAst(
  value: unknown,
  visit: (node: AstNode, ancestors: readonly AstNode[]) => void,
  ancestors: readonly AstNode[] = [],
) {
  if (typeof value !== "object" || value === null) return;
  const node = isAstNode(value) ? value : null;
  if (node) visit(node, ancestors);
  const childAncestors = node ? [...ancestors, node] : ancestors;
  for (const child of Object.values(value)) walkAst(child, visit, childAncestors);
}

// For each call to `callee`, whether it runs only on the cache-miss branch of
// an `if ($[n] !== dep)` block rather than on every render.
function compilerCachedCalls(file: string, callee: string): boolean[] {
  const result = compileLikeMetro(file, [], { ast: true });
  const cached: boolean[] = [];
  if (typeof result !== "object" || result === null || !("ast" in result)) return cached;
  walkAst(result.ast, (node, ancestors) => {
    if (node.type !== "CallExpression" || !isAstNode(node.callee)) return;
    if (node.callee.type !== "Identifier" || node.callee.name !== callee) return;
    cached.push(
      ancestors.some(
        (ancestor, index) =>
          ancestor.type === "IfStatement" &&
          ancestors[index + 1] === ancestor.consequent &&
          JSON.stringify(ancestor.test).includes('"name":"$"'),
      ),
    );
  });
  return cached;
}

// What each of these builds on every render, outside the compiler's cache. They render on each
// update of a working thread or once per list row, so a new handler or element here re-renders
// what it is passed to. Lower a number when it drops; raising one needs a reason in review.
const RENDER_ALLOCATION_BUDGETS: Record<string, number> = {
  "src/features/threads/ThreadRouteScreen.tsx#ThreadRouteContent": 0,
  "src/features/threads/ThreadDetailScreen.tsx#ThreadDetailBody": 0,
  "src/features/threads/ThreadHeader.tsx#ThreadHeader": 0,
  "src/features/threads/ThreadFeed.tsx#ThreadFeed": 3,
  "src/features/threads/ThreadComposer.tsx#ThreadComposer": 0,
  "src/features/threads/floating-working-control.tsx#FloatingWorkingControl": 0,
  "src/components/ComposerEditor.tsx#ComposerEditor": 3,
  "src/state/use-thread-composer-state.ts#useThreadComposerState": 0,
  "src/features/threads/thread-list-v2-items.tsx#ThreadListV2Row": 68,
  "src/features/threads/thread-list-v2-items.tsx#ThreadListV2PendingRow": 31,
};

const isFunctionNode = (node: AstNode) =>
  node.type === "ArrowFunctionExpression" ||
  node.type === "FunctionExpression" ||
  node.type === "FunctionDeclaration";

const identifierName = (value: unknown) =>
  isAstNode(value) && value.type === "Identifier" && typeof value.name === "string"
    ? value.name
    : null;

const readsCompilerCache = (value: unknown) => JSON.stringify(value).includes('"name":"$"');

function allocationKind(node: AstNode) {
  if (isFunctionNode(node)) return "function";
  if (node.type === "ObjectExpression") return "object";
  if (node.type === "ArrayExpression") return "array";
  if (node.type === "NewExpression") return "new";
  const callee = node.type === "CallExpression" ? identifierName(node.callee) : null;
  return callee === "_jsx" || callee === "_jsxs" ? "element" : null;
}

function isHookCall(node: AstNode) {
  if (node.type !== "CallExpression" || !isAstNode(node.callee)) return false;
  const callee = node.callee.type === "MemberExpression" ? node.callee.property : node.callee;
  return /^use[A-Z]/.test(identifierName(callee) ?? "");
}

// `$[n] !== props` in a guard: the block is rebuilt whenever the parent renders.
function wholePropsGuards(test: unknown, props: string): number {
  if (!isAstNode(test)) return 0;
  if (test.type === "LogicalExpression") {
    return wholePropsGuards(test.left, props) + wholePropsGuards(test.right, props);
  }
  return test.type === "BinaryExpression" && identifierName(test.right) === props ? 1 : 0;
}

// The values a compiled function builds on every render, as `line:kind`. Hook arguments and
// branches the compiler did not write are left out: effects and conditional work are not memo loss.
function renderAllocations(fn: AstNode, name: string): string[] {
  const found: string[] = [];
  const params = Array.isArray(fn.params) ? fn.params : [];
  const props = /^[A-Z]/.test(name) ? identifierName(params[0]) : null;
  const visit = (value: unknown) => {
    if (typeof value !== "object" || value === null) return;
    if (!isAstNode(value)) {
      for (const child of Object.values(value)) visit(child);
      return;
    }
    if (value.type === "IfStatement") {
      if (!readsCompilerCache(value.test)) return visit(value.test);
      const guards = props === null ? 0 : wholePropsGuards(value.test, props);
      for (let guard = 0; guard < guards; guard++) found.push("whole-props");
      return visit(value.alternate);
    }
    const kind = allocationKind(value);
    if (kind) found.push(`${value.loc?.start?.line ?? "?"}:${kind}`);
    if (isFunctionNode(value)) return;
    if (isHookCall(value)) return visit(value.callee);
    for (const [key, child] of Object.entries(value)) if (key !== "loc") visit(child);
  };
  visit(fn.body);
  return found;
}

function compiledFunctionAllocations(file: string): Map<string, string[]> {
  const result = compileLikeMetro(file, [], { ast: true });
  const allocations = new Map<string, string[]>();
  if (typeof result !== "object" || result === null || !("ast" in result)) return allocations;
  walkAst(result.ast, (node, ancestors) => {
    if (!isFunctionNode(node) || !isAstNode(node.body) || !Array.isArray(node.body.body)) return;
    if (!/"name":"_c"/.test(JSON.stringify(node.body.body[0]?.declarations?.[0]?.init?.callee))) {
      return;
    }
    const declarator = ancestors.findLast((ancestor) => ancestor.type === "VariableDeclarator");
    const name = identifierName(node.id) ?? identifierName(declarator?.id);
    if (name !== null) allocations.set(name, renderAllocations(node, name));
  });
  return allocations;
}

const allocationsByFile = new Map<string, Map<string, string[]>>();

function functionAllocations(target: string) {
  const [file = "", name = ""] = target.split("#");
  const allocations = allocationsByFile.get(file) ?? compiledFunctionAllocations(file);
  allocationsByFile.set(file, allocations);
  return allocations.get(name);
}

describe("React Compiler coverage", () => {
  it("Metro's transformer applies the compiler", async () => {
    const { transformer } = await metroConfig;
    const metroTransformer = requireFromMobile(transformer.babelTransformerPath);
    const filename = NodePath.join(
      mobileRoot,
      "src/features/archive/ArchivedThreadsRouteScreen.tsx",
    );
    const transformed = (reactCompiler: string) => {
      const { ast } = metroTransformer.transform({
        filename,
        src: NodeFS.readFileSync(filename, "utf8"),
        plugins: [],
        options: {
          dev: false,
          hot: false,
          platform: "ios",
          projectRoot: mobileRoot,
          type: "module",
          enableBabelRCLookup: false,
          experimentalImportSupport: false,
          customTransformOptions: { reactCompiler, routerRoot: "app" },
        },
      });
      return JSON.stringify(ast.program.body);
    };
    expect(transformed("true")).toContain("react/compiler-runtime");
    expect(transformed("false")).not.toContain("react/compiler-runtime");
  });

  it.each(sourceFiles)("compiles %s", (file) => {
    const bailouts = compilerBailouts(file);
    if (file in KNOWN_BAILOUTS) {
      expect(bailouts, `${file} compiles now; remove it from KNOWN_BAILOUTS`).not.toEqual([]);
    } else {
      expect(bailouts).toEqual([]);
    }
  });

  // The compiler once silently dropped this cache, so every
  // ThreadFeed render rebuilt the feed and handed LegendList a new data array.
  it("caches the thread feed derivation", () => {
    expect(
      compilerCachedCalls("src/features/threads/ThreadFeed.tsx", "deriveThreadFeedPresentation"),
    ).toEqual([true]);
  });

  // The compiler once silently dropped these caches, so the new-task context
  // value changed on every provider render and re-rendered every consumer.
  it("caches the new-task model options and host environments", () => {
    const file = "src/features/threads/new-task-flow-provider.tsx";
    expect(compilerCachedCalls(file, "buildModelOptions")).toEqual([true]);
    expect(compilerCachedCalls(file, "repositoryGroupingKeyOf")).toEqual([true, true]);
  });

  // An uncached registration would re-register the workspace inspector on every render.
  it("registers the workspace inspector only when its renderer changes", () => {
    expect(
      compilerCachedCalls(
        "src/features/layout/AdaptiveWorkspaceLayout.tsx",
        "registerWorkspaceInspector",
      ),
    ).toEqual([true]);
  });

  it.each(Object.entries(RENDER_ALLOCATION_BUDGETS))(
    "%s builds %i values on every render",
    (target, budget) => {
      const allocations = functionAllocations(target);
      expect(allocations, `${target} is not a compiled function`).toBeDefined();
      expect(allocations, `built on every render: ${allocations?.join(" ")}`).toHaveLength(budget);
    },
  );

  it("lists only files that exist", () => {
    expect(Object.keys(KNOWN_BAILOUTS).filter((file) => !sourceFiles.includes(file))).toEqual([]);
  });
});
