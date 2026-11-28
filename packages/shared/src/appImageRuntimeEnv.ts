const APPIMAGE_RUNTIME_ENV_KEYS: ReadonlySet<string> = new Set([
  "APPIMAGE",
  "APPDIR",
  "ARGV0",
  "OWD",
]);
// ld.so also splits LD_LIBRARY_PATH on semicolons.
const APPIMAGE_PATH_LIKE_ENV_SEPARATORS: ReadonlyMap<string, RegExp> = new Map([
  ["PATH", /:/],
  ["LD_LIBRARY_PATH", /[:;]/],
  ["XDG_DATA_DIRS", /:/],
  ["GSETTINGS_SCHEMA_DIR", /:/],
]);

interface NormalizedPosixPath {
  readonly absolute: boolean;
  readonly parts: readonly string[];
}

function normalizePosixPath(value: string): NormalizedPosixPath {
  return {
    absolute: value.startsWith("/"),
    parts: value.split("/").filter((part) => part !== "" && part !== "."),
  };
}

function isPathSegmentUnderAppDir(segment: string, appDir: NormalizedPosixPath): boolean {
  const normalized = normalizePosixPath(segment);
  // Where ".." lands depends on symlinks, so such an entry may point outside the mount; keep it.
  if (
    normalized.absolute !== appDir.absolute ||
    normalized.parts.length < appDir.parts.length ||
    normalized.parts.includes("..")
  ) {
    return false;
  }
  return appDir.parts.every((part, index) => normalized.parts[index] === part);
}

/**
 * What a child command should get for each variable of an AppImage's environment, `undefined` to
 * drop it: the mount's paths would make it resolve against the mount instead of the user's system.
 * `undefined` when `env` is not an AppImage's.
 */
export function appImageRuntimeScrub(
  env: NodeJS.ProcessEnv,
): ((name: string, value: string) => string | undefined) | undefined {
  if (env.APPIMAGE === undefined && env.APPDIR === undefined) return undefined;

  const appDir = env.APPDIR ? normalizePosixPath(env.APPDIR) : undefined;
  return (name, value) => {
    if (APPIMAGE_RUNTIME_ENV_KEYS.has(name)) return undefined;
    const separator = APPIMAGE_PATH_LIKE_ENV_SEPARATORS.get(name);
    if (!appDir || appDir.parts.length === 0 || separator === undefined) return value;
    const segments = value.split(separator);
    const kept = segments.filter((segment) => !isPathSegmentUnderAppDir(segment, appDir));
    if (kept.length === segments.length) return value;
    if (kept.length === 0) return undefined;
    // Empty entries mean the current directory; a lone one must stay ":" since "" means none.
    return kept.length === 1 && kept[0] === "" ? ":" : kept.join(":");
  };
}

export function stripAppImageRuntimeEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const scrub = appImageRuntimeScrub(env);
  if (scrub === undefined) return env;

  const scrubbed: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(env)) {
    const kept = value === undefined ? undefined : scrub(name, value);
    if (kept !== undefined) scrubbed[name] = kept;
  }
  return scrubbed;
}
