// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";

import { isNetworkPath } from "@onicode/shared/path";
import type * as Path from "effect/Path";

import { isWithinRoot } from "./KnownWorkspaces.ts";

// Real link chains are short; a longer one is refused rather than followed.
const MAX_LINK_HOPS = 40;

// `\\?\` and `\??\` name a drive or UNC path verbatim, so those map back to one unless a `..`
// is left for the filesystem to read. Any other NT or device namespace path (`\\.\`,
// `\??\GLOBALROOT`, a volume) may reach a share the walk cannot see: null.
const fromNtNamespace = (value: string): string | null => {
  if (!/^[\\/](?:[?.][?\\/]|[\\/][?.][\\/])/.test(value)) return value;
  const verbatim = /^(?:[\\/]{2}\?|[\\/]\?\?)[\\/](.*)$/s.exec(value)?.[1];
  if (verbatim === undefined || /(?:^|[\\/])\.\.?(?:[\\/]|$)/.test(verbatim)) return null;
  if (/^[A-Za-z]:[\\/]/.test(verbatim)) return verbatim;
  const unc = /^UNC[\\/](.+)$/is.exec(verbatim)?.[1];
  return unc === undefined ? null : `\\\\${unc}`;
};

/**
 * The network path outside `allowedRoot` that resolving `filePath` (below `from`, a path with no
 * links on it, when relative) would touch, read from links without following them. Windows signs
 * in to a share with NTLM as soon as realpath, stat or open reaches it. POSIX reads `//` as `/`.
 */
export async function networkPathReachedBy(
  path: Path.Path,
  filePath: string,
  options: { readonly from?: string | undefined; readonly allowedRoot?: string | undefined } = {},
): Promise<string | null> {
  const windows = path.sep === "\\";
  const separators = windows ? /[\\/]+/ : /\/+/;
  const segments = (value: string) => value.split(separators).filter(Boolean).reverse();
  const allowedRoot =
    options.allowedRoot !== undefined && windows
      ? (fromNtNamespace(options.allowedRoot) ?? options.allowedRoot)
      : options.allowedRoot;
  let canonicalRoot: Promise<string> | undefined;
  const refused = async (candidate: string) => {
    if (allowedRoot === undefined) return true;
    const target = path.resolve(candidate);
    if (isWithinRoot(path, target, path.resolve(allowedRoot))) return false;
    // A local root that is itself a link to a share still allows that share.
    if (isNetworkPath(allowedRoot)) return true;
    canonicalRoot ??= NodeFSP.realpath(allowedRoot).catch(() => path.resolve(allowedRoot));
    return !isWithinRoot(path, target, await canonicalRoot);
  };
  let current: string;
  const pending: Array<string> = [];
  if (options.from === undefined) {
    const placed = windows ? fromNtNamespace(filePath) : filePath;
    if (placed === null) return filePath;
    if (isNetworkPath(placed) && (await refused(placed))) return placed;
    if (!windows) return null;
    // Windows collapses `..` before it follows links, in paths and in link targets alike.
    const absolute = path.resolve(placed);
    current = path.parse(absolute).root;
    pending.push(...segments(absolute.slice(current.length)));
  } else {
    if (!windows) return null;
    current = options.from;
    pending.push(...segments(path.normalize(filePath)));
  }
  for (let hops = 0; ;) {
    const name = pending.pop();
    if (name === undefined) return null;
    if (name === ".") continue;
    if (name === "..") {
      current = path.dirname(current);
      continue;
    }
    const next = path.join(current, name);
    let link: string | null;
    try {
      link = fromNtNamespace(await NodeFSP.readlink(next));
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      // Nothing past a missing entry can be followed; any other failure means it is no link.
      if (code === "ENOENT" || code === "ENOTDIR") return null;
      current = next;
      continue;
    }
    if (link === null) return next;
    // A link on an allowed share is walked on: it may point at another share.
    if (isNetworkPath(link) && (await refused(link))) {
      return [link.replace(/[\\/]+$/, ""), ...pending.toReversed()].join(path.sep);
    }
    // Past this, or through a drive-relative target that follows another drive's working
    // directory, the walk cannot tell where the link leads, so it refuses.
    if (++hops > MAX_LINK_HOPS || /^[A-Za-z]:(?![\\/])/.test(link)) return next;
    const root = path.isAbsolute(link) ? path.parse(link).root : "";
    // A root-relative target such as `\dir` stays on the link's own drive or share.
    if (root !== "") current = /^[\\/]$/.test(root) ? path.parse(current).root : root;
    pending.push(...segments(path.normalize(link.slice(root.length))));
  }
}
