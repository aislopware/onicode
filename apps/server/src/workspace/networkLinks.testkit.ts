// @effect-diagnostics nodeBuiltinImport:off
import type * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";

/** A share no test may reach; paths onto it show up in `reachedShare`. */
export const ATTACKER_SHARE = "\\\\attacker.invalid\\share";

const onAttackerShare = (target: string) => /attacker\.invalid/i.test(target);

const errno = (code: string) => Object.assign(new Error(code), { code });

// One spelling per path: normalized, without a trailing separator past the root.
const key = (target: string) => {
  const normalized = NodePath.win32.normalize(target);
  return normalized.length > NodePath.win32.parse(normalized).root.length
    ? normalized.replace(/\\$/, "")
    : normalized;
};

/**
 * A Windows tree in memory, so the network-link walk runs on any host. Links keep their target
 * as typed. Like Windows, readlink follows every link but the last name, and stat, realpath and
 * the rest follow them all; every path a call follows is recorded.
 */
export const fakeWindowsTree = (tree: {
  readonly links?: Readonly<Record<string, string>>;
  readonly files?: Readonly<Record<string, string>>;
  readonly directories?: ReadonlyArray<string>;
}) => {
  const links = new Map(Object.entries(tree.links ?? {}).map(([at, to]) => [key(at), to]));
  const files = new Map(Object.entries(tree.files ?? {}).map(([at, text]) => [key(at), text]));
  const directories = new Set((tree.directories ?? []).map(key));
  for (const entry of [...links.keys(), ...files.keys(), ...directories]) {
    for (let parent = key(NodePath.win32.dirname(entry)); !directories.has(parent);) {
      directories.add(parent);
      parent = key(NodePath.win32.dirname(parent));
    }
  }
  const followed: Array<string> = [];
  const follow = (target: string) => {
    const normalized = key(target);
    let resolved = NodePath.win32.parse(normalized).root;
    followed.push(resolved);
    for (const name of normalized.slice(resolved.length).split("\\").filter(Boolean)) {
      resolved = key(NodePath.win32.join(resolved, name));
      followed.push(resolved);
      for (let hops = 0; hops < 64; hops += 1) {
        const link = links.get(resolved);
        if (link === undefined) break;
        resolved = key(NodePath.win32.resolve(NodePath.win32.dirname(resolved), link));
        followed.push(resolved);
      }
    }
    return resolved;
  };
  const exists = (target: string) =>
    files.has(target) || directories.has(target) || onAttackerShare(target);
  const kind = (target: string) => {
    const real = follow(target);
    if (files.has(real)) return "file" as const;
    return exists(real) ? ("directory" as const) : undefined;
  };
  return {
    kind,
    /** `target` with every link on it followed, or undefined when nothing is there. */
    real: (target: string) => {
      const real = follow(target);
      return exists(real) ? real : undefined;
    },
    contents: (target: string) => files.get(follow(target)),
    children: (directory: string) => {
      const real = follow(directory);
      return [...links.keys(), ...files.keys(), ...directories]
        .filter((entry) => entry !== real && key(NodePath.win32.dirname(entry)) === real)
        .map((entry) => ({
          name: NodePath.win32.basename(entry),
          type: links.has(entry)
            ? ("SymbolicLink" as const)
            : files.has(entry)
              ? ("File" as const)
              : ("Directory" as const),
        }));
    },
    readlink: (async (target: string) => {
      const entry = key(
        NodePath.win32.join(
          follow(NodePath.win32.dirname(target)),
          NodePath.win32.basename(target),
        ),
      );
      const link = links.get(entry);
      if (link !== undefined) return link;
      throw errno(exists(entry) ? "EINVAL" : "ENOENT");
    }) as typeof NodeFSP.readlink,
    realpath: (async (target: string) => {
      const real = follow(target);
      if (!exists(real)) throw errno("ENOENT");
      return real;
    }) as typeof NodeFSP.realpath,
    stat: (async (target: string) => {
      const found = kind(target);
      if (found === undefined) throw errno("ENOENT");
      return { isDirectory: () => found === "directory", isFile: () => found === "file" };
    }) as unknown as typeof NodeFSP.stat,
    /** Every path a call followed onto the attacker's share. */
    reachedShare: () => followed.filter(onAttackerShare),
  };
};
