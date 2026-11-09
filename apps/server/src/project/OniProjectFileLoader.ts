import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { ONICODE_PROJECT_FILE_NAME, type OniProjectFile } from "@onicode/contracts";
import { OniProjectFileFromJson } from "@onicode/shared/oniProjectFile";

const decodeOniProjectFileJson = Schema.decodeEffect(OniProjectFileFromJson);

export class OniProjectFileLoadError extends Schema.TaggedError<OniProjectFileLoadError>()(
  "OniProjectFileLoadError",
  {
    operation: Schema.Literals(["read", "decode"]),
    workspaceRoot: Schema.String,
    filePath: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to ${this.operation} ${ONICODE_PROJECT_FILE_NAME} at ${this.filePath}.`;
  }
}

export class OniProjectFileLoader extends Context.Service<
  OniProjectFileLoader,
  {
    /** Never fails; missing/unreadable/invalid files return none, with warnings for invalid files. */
    readonly load: (workspaceRoot: string) => Effect.Effect<Option.Option<OniProjectFile>>;
  }
>()("onicode/project/OniProjectFileLoader") {}

const logOniProjectFileLoadError = (error: OniProjectFileLoadError) =>
  Effect.logWarning(error).pipe(
    Effect.annotateLogs({
      operation: error.operation,
      workspaceRoot: error.workspaceRoot,
      filePath: error.filePath,
      errorTag: error._tag,
    }),
  );

/** @public */
export const make = Effect.gen(function* () {
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;

  const load: OniProjectFileLoader["Service"]["load"] = Effect.fn("OniProjectFileLoader.load")(
    function* (workspaceRoot) {
      const filePath = path.join(workspaceRoot, ONICODE_PROJECT_FILE_NAME);
      const raw = yield* fileSystem.readFileString(filePath).pipe(
        Effect.asSome,
        Effect.catchTags({
          PlatformError: (error) =>
            error.reason._tag === "NotFound"
              ? Effect.succeed(Option.none<string>())
              : logOniProjectFileLoadError(
                  new OniProjectFileLoadError({
                    operation: "read",
                    workspaceRoot,
                    filePath,
                    cause: error,
                  }),
                ).pipe(Effect.as(Option.none<string>())),
        }),
      );
      if (Option.isNone(raw)) {
        return Option.none<OniProjectFile>();
      }
      return yield* decodeOniProjectFileJson(raw.value).pipe(
        Effect.asSome,
        Effect.catchTags({
          SchemaError: (error) =>
            logOniProjectFileLoadError(
              new OniProjectFileLoadError({
                operation: "decode",
                workspaceRoot,
                filePath,
                cause: error,
              }),
            ).pipe(Effect.as(Option.none<OniProjectFile>())),
        }),
      );
    },
  );

  return OniProjectFileLoader.of({ load });
});

export const layer = Layer.effect(OniProjectFileLoader, make);
