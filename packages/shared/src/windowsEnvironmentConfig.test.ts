import { assert, it } from "@effect/vitest";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";

import { windowsEnvironmentVariable } from "./windowsEnvironmentConfig.ts";

it.effect("reads a Windows variable from a copied environment in any case", () =>
  Effect.gen(function* () {
    const read = (name: string) =>
      windowsEnvironmentVariable(name).pipe(
        Effect.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                SYSTEMROOT: "D:\\WinNT",
                Programfiles: "D:\\Apps",
                ProgramFiles_x86: "exact",
                PROGRAMFILES_X86: "other",
                Processor_Architecture: "ARM64",
              },
            }),
          ),
        ),
      );

    assert.strictEqual(yield* read("SystemRoot"), "D:\\WinNT");
    assert.strictEqual(yield* read("ProgramFiles"), "D:\\Apps");
    assert.strictEqual(yield* read("ProgramFiles_x86"), "exact");
    assert.strictEqual(yield* read("PROCESSOR_ARCHITECTURE"), "ARM64");
    assert.strictEqual(yield* read("windir"), undefined);
  }),
);
