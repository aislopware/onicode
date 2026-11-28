// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";

import { isNetworkPath } from "@onicode/shared/path";
import type * as Path from "effect/Path";

import { isWithinRoot } from "./KnownWorkspaces.ts";

const MAX_LINK_HOPS = 40;

/**
 * The network path outside `allowedRoot` that resolving `filePath` would touch, found by reading
 * links rather than following them, or null. Windows signs in to a share with the user's NTLM
 * credentials as soon as realpath, stat or open reaches it, so this runs before any of them.
 * A relative `filePath` resolves below `from`, a real directory.
 */
export async function networkPathReachedBy(
  path: Path.Path,
  filePath: string,
  options: { readonly from?: string | undefined; readonly allowedRoot?: string | undefined } = {},
): Promise<string | null> {
  const separators = path.sep === "\\" ? /[\\/]+/ : /\/+/;
  const segments = (value: string) => value.split(separators).filter(Boolean).reverse();
  const refused = (candidate: string) =>
    options.allowedRoot === undefined ||
    !isWithinRoot(path, path.resolve(candidate), path.resolve(options.allowedRoot));
  let current: string;
  const pending: Array<string> = [];
  if (options.from === undefined) {
    // An allowed share is the caller's own project, so nothing on it is walked.
    if (isNetworkPath(filePath)) return refused(filePath) ? filePath : null;
    // Windows collapses `..` before it follows links; POSIX follows the link first.
    const absolute = path.sep === "\\" ? path.resolve(filePath) : filePath;
    current = path.parse(absolute).root;
    pending.push(...segments(absolute.slice(current.length)));
  } else {
    current = options.from;
    pending.push(...segments(filePath));
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
    if (isNetworkPath(link)) {
      return refused(link) ? [link, ...pending.toReversed()].join(path.sep) : null;
    }
    // Leave a link loop for realpath to report.
    if (++hops > MAX_LINK_HOPS) return null;
    const root = path.isAbsolute(link) ? path.parse(link).root : "";
    if (root !== "") current = root;
    pending.push(...segments(link.slice(root.length)));
  }
}
