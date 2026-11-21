import type { ProviderInstanceEnvironment } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";

import { expandHomeVariable } from "./pathExpansion.ts";

/**
 * A copy of `base` that keeps reading it for every name the copy has not written or deleted.
 * Drivers build their environment once, and the desktop's shell snapshot (PATH, CA files) reaches
 * the host after that. `hidden` names read as unset from `base`. Names fold case on Windows.
 * It is a Proxy, which structured clone (postMessage) refuses: spread it first.
 */
export const environmentView = (
  base: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
  hidden: (name: string) => boolean = () => false,
): NodeJS.ProcessEnv => {
  const windows = platform === "win32";
  const fold = (name: string) => (windows ? name.toUpperCase() : name);
  // `undefined` records a delete.
  const local = new Map<string, { readonly name: string; readonly value: string | undefined }>();
  const own = (name: string) => {
    const value = Object.hasOwn(base, name) ? base[name] : undefined;
    return typeof value === "string" ? value : undefined;
  };
  const read = (name: string | symbol) => {
    if (typeof name !== "string") return undefined;
    const written = local.get(fold(name));
    if (written !== undefined) return written.value;
    if (hidden(name)) return undefined;
    const value = own(name);
    if (value !== undefined || !windows) return value;
    const spelling = Object.keys(base).find((key) => fold(key) === fold(name));
    return spelling === undefined ? undefined : own(spelling);
  };
  const write = (name: string | symbol, value: unknown) => {
    if (typeof name !== "string") return false;
    local.set(fold(name), { name, value: value === undefined ? undefined : String(value) });
    return true;
  };
  return new Proxy<NodeJS.ProcessEnv>(
    {},
    {
      get: (_target, name) => read(name),
      has: (_target, name) => read(name) !== undefined,
      set: (_target, name, value) => write(name, value),
      defineProperty: (_target, name, descriptor) =>
        "value" in descriptor && write(name, descriptor.value),
      deleteProperty: (_target, name) => write(name, undefined),
      ownKeys: () => [
        ...Object.keys(base).filter(
          (name) => !local.has(fold(name)) && !hidden(name) && own(name) !== undefined,
        ),
        ...[...local.values()].flatMap(({ name, value }) => (value === undefined ? [] : [name])),
      ],
      getOwnPropertyDescriptor: (_target, name) => {
        const value = read(name);
        return value === undefined
          ? undefined
          : { value, enumerable: true, configurable: true, writable: true };
      },
    },
  );
};

export const mergeProviderInstanceEnvironment = Effect.fn(function* (
  environment: ProviderInstanceEnvironment | undefined,
  baseEnv: NodeJS.ProcessEnv = process.env,
) {
  if (!environment || environment.length === 0) {
    return baseEnv;
  }

  const home = yield* HostProcess.HomeDirectory;
  const platform = yield* HostProcess.Platform;
  const next = environmentView(baseEnv, platform);
  for (const variable of environment) {
    // An unresolved secret reference carries no value; the registry resolves it for the provider.
    if (typeof variable.value !== "string") continue;
    // A child gets the first spelling of a Windows name in sort order. Upper case sorts first, so
    // the configured value wins even where a spawn merges the parent environment back in.
    const name = platform === "win32" ? variable.name.toUpperCase() : variable.name;
    next[name] = expandHomeVariable(name, variable.value, home);
  }
  return next;
});
