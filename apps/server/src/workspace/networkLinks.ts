// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";

import { isNetworkPath } from "@onicode/shared/path";
import type * as Path from "effect/Path";

import { isWithinRoot } from "./KnownWorkspaces.ts";

// Real link chains are short; a longer one is refused rather than followed.
const MAX_LINK_HOPS = 40;

/**
 * The network path outside `allowedRoot` that resolving `filePath` (below `from` when relative)
 * would touch, read from links without following them. Windows signs in to a share with NTLM as
 * soon as realpath, stat or open reaches it. POSIX reads `//` as `/`, so only Windows walks.
 */
export async function networkPathReachedBy(
  path: Path.Path,
  filePath: string,
  options: { readonly from?: string | undefined; readonly allowedRoot?: string | undefined } = {},
): Promise<string | null> {
  const separators = path.sep === "\\" ? /[\\/]+/ : /\/+/;
  const segments = (value: string) => value.split(separators).filter(Boolean).reverse();
  const allowedRoot = options.allowedRoot;
  let canonicalRoot: Promise<string> | undefined;
  const refused = async (candidate: string) => {
    if (allowedRoot === undefined) return true;
    const target = path.resolve(candidate);
    if (isWithinRoot(path, target, path.resolve(allowedRoot))) return false;
    // A root that is itself a link to a share still allows that share.
    canonicalRoot ??= NodeFSP.realpath(allowedRoot).catch(() => path.resolve(allowedRoot));
    return !isWithinRoot(path, target, await canonicalRoot);
  };
  let current: string;
  const pending: Array<string> = [];
  if (options.from === undefined) {
    if (isNetworkPath(filePath) && (await refused(filePath))) return filePath;
    if (path.sep !== "\\") return null;
    // Windows collapses `..` before it follows links, in paths and in link targets alike.
    const absolute = path.resolve(filePath);
    current = path.parse(absolute).root;
    pending.push(...segments(absolute.slice(current.length)));
  } else {
    if (path.sep !== "\\") return null;
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
    let link: string;
    try {
      link = await NodeFSP.readlink(next);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      // Nothing past a missing entry can be followed; any other failure means it is no link.
      if (code === "ENOENT" || code === "ENOTDIR") return null;
      current = next;
      continue;
    }
    // A link on an allowed share is walked on: it may point at another share.
    if (isNetworkPath(link) && (await refused(link))) {
      return [link, ...pending.toReversed()].join(path.sep);
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
