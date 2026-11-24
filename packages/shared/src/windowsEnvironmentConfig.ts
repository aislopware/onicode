import * as Config from "effect/Config";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";

/**
 * A Windows variable from the configured environment, in any case. The default provider copies
 * the environment, so its names lose Windows' case-insensitivity; it nests names by `_`.
 */
export const windowsEnvironmentVariable = Effect.fn("windowsEnvironmentVariable")(
  function* (name: string) {
    // The plain read first: it honours fallback providers the walk below cannot see.
    const exact = yield* Config.option(Config.String(name));
    if (Option.isSome(exact)) return exact.value;
    const provider = yield* ConfigProvider.ConfigProvider;
    const find: (
      path: ReadonlyArray<string>,
      rest: ReadonlyArray<string>,
    ) => Effect.Effect<string | undefined, ConfigProvider.SourceError> = Effect.fn(
      "windowsEnvironmentVariable.find",
    )(function* (path, rest) {
      const node = yield* provider.load(path);
      const [segment, ...after] = rest;
      if (segment === undefined) return node?.value;
      const keys = node?._tag === "Record" ? [...node.keys] : [];
      const wanted = segment.toUpperCase();
      // The exact spelling first, then any other.
      const candidates = [
        ...keys.filter((key) => key === segment),
        ...keys.filter((key) => key !== segment && key.toUpperCase() === wanted),
      ];
      for (const candidate of candidates) {
        const value = yield* find([...path, candidate], after);
        if (value !== undefined) return value;
      }
      return undefined;
    });
    return yield* find([], name.split("_"));
  },
  Effect.orElseSucceed(() => undefined),
);
