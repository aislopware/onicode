import {
  CustomModelSetting,
  makeBinaryPathSetting,
  makeProviderSettingsSchema,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

export const CommandCodeSettings = makeProviderSettingsSchema(
  {
    enabled: Schema.Boolean.pipe(
      Schema.withDecodingDefault(Effect.succeed(false)),
      Schema.annotateKey({ providerSettingsForm: { hidden: true } }),
    ),
    // `command-code`, not the shorter `cmd`, which is the Windows shell.
    binaryPath: makeBinaryPathSetting("command-code").pipe(
      Schema.annotateKey({
        title: "Binary path",
        description: "Path to the Command Code CLI binary.",
        providerSettingsForm: { placeholder: "command-code", clearWhenEmpty: "omit" },
      }),
    ),
    customModels: Schema.Array(CustomModelSetting).pipe(
      Schema.withDecodingDefault(Effect.succeed([])),
      Schema.annotateKey({ providerSettingsForm: { hidden: true } }),
    ),
  },
  {
    order: ["binaryPath"],
  },
);
export type CommandCodeSettings = typeof CommandCodeSettings.Type;
