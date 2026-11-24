import type { ProviderInstanceEnvironment } from "@onicode/contracts";
import { appImageRuntimeScrub } from "@onicode/shared/appImageRuntimeEnv";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";

import { expandHomeVariable } from "./pathExpansion.ts";

/**
 * A copy of `base` that keeps reading what it has not written: the desktop's shell snapshot reaches
 * the host after drivers are built. `hidden` names read as unset; `rewrite` maps what `base` gives.
 * Names fold case on Windows. It is a Proxy, which structured clone refuses: spread it first.
 */
export const environmentView = (
  base: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
  hidden: (name: string) => boolean = () => false,
  rewrite: (name: string, value: string) => string | undefined = (_name, value) => value,
): NodeJS.ProcessEnv => {
  const windows = platform === "win32";
  const fold = (name: string) => (windows ? name.toUpperCase() : name);
  // `undefined` records a delete.
  const local = new Map<string, { readonly name: string; readonly value: string | undefined }>();
  const own = (name: string) => {
    const value = Object.hasOwn(base, name) ? base[name] : undefined;
    return typeof value === "string" ? rewrite(name, value) : undefined;
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
  const platform = yield* HostProcess.Platform;
  // Commands the agent runs inherit this env; the AppImage mount's libraries would shadow the system's.
  const scrub = appImageRuntimeScrub(baseEnv);
  const hostEnv =
    scrub === undefined ? baseEnv : environmentView(baseEnv, platform, undefined, scrub);
  if (!environment || environment.length === 0) {
    return hostEnv;
  }

  const home = yield* HostProcess.HomeDirectory;
  const next = environmentView(hostEnv, platform);
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
