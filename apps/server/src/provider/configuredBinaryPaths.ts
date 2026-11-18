import type { ProviderInstanceConfig } from "@onicode/contracts";
import { mergeProviderInstanceEnvironment } from "@onicode/provider-core/server/instanceEnvironment";
import { expandHomePath } from "@onicode/provider-core/server/pathExpansion";
import * as HostProcess from "@onicode/shared/HostProcess";
import { resolveCommandPath } from "@onicode/shared/shell";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

const hasBinaryPath = Schema.is(Schema.Struct({ binaryPath: Schema.String }));

/** Each instance's custom executable as written and as its own PATH resolves it. */
export const configuredBinaryPaths = Effect.fn("configuredBinaryPaths")(function* (
  entries: Iterable<ProviderInstanceConfig>,
) {
  const home = yield* HostProcess.HomeDirectory;
  const paths = yield* Effect.forEach(
    entries,
    Effect.fnUntraced(function* (entry) {
      if (!hasBinaryPath(entry.config) || !entry.config.binaryPath.trim()) return [];
      const binaryPath = expandHomePath(entry.config.binaryPath.trim(), home);
      const resolved = yield* resolveCommandPath(binaryPath, {
        env: yield* mergeProviderInstanceEnvironment(entry.environment),
      });
      return resolved === null ? [binaryPath] : [binaryPath, resolved];
    }),
  );
  return paths.flat();
});
