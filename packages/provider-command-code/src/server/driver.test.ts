import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import type * as EffectAcpSchema from "effect-acp/compat";
import * as EffectAcpErrors from "effect-acp/errors";

import { CommandCodeSettings } from "../settings.ts";
import {
  buildCommandCodeAcpSpawnInput,
  commandCodeSessionModeForPolicy,
  EMPTY_COMMAND_CODE_CATALOG,
  learnCommandCodeCatalog,
} from "./acpSupport.ts";
import {
  buildInitialCommandCodeProviderSnapshot,
  commandCodeAuthFromProbe,
  commandCodeModels,
} from "./status.ts";

const decodeSettings = Schema.decodeUnknownSync(CommandCodeSettings);

// Shapes recorded from `command-code acp` 1.88.5.
const modelOption = (currentValue: string): EffectAcpSchema.SessionConfigOption => ({
  id: "model",
  name: "Model",
  category: "model",
  type: "select",
  currentValue,
  options: [
    { value: "deepseek/deepseek-v4.1-flash", name: "DeepSeek V4.1 Flash" },
    { value: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", description: "Anthropic" },
  ],
});
const effortOption = (
  values: ReadonlyArray<string>,
  currentValue: string,
): EffectAcpSchema.SessionConfigOption => ({
  id: "effort",
  name: "Effort",
  category: "thought_level",
  type: "select",
  currentValue,
  options: values.map((value) => ({ value, name: value })),
});

describe("CommandCodeDriver", () => {
  it.effect("reports no text generation, so preflight hands that work to another provider", () =>
    Effect.gen(function* () {
      const snapshot = yield* buildInitialCommandCodeProviderSnapshot(
        decodeSettings({}),
        EMPTY_COMMAND_CODE_CATALOG,
      );
      assert.isFalse(snapshot.supportsTextGeneration);
    }),
  );

  it("launches `command-code acp` from the configured binary", () => {
    assert.deepEqual(buildCommandCodeAcpSpawnInput(decodeSettings({}), "/work"), {
      command: "command-code",
      args: ["acp"],
      cwd: "/work",
    });
    const custom = buildCommandCodeAcpSpawnInput(
      decodeSettings({ binaryPath: "/opt/bin/cmdc" }),
      "/work",
      { COMMAND_CODE_API_KEY: "key" },
    );
    assert.equal(custom.command, "/opt/bin/cmdc");
    assert.deepEqual(custom.env, { COMMAND_CODE_API_KEY: "key" });
  });

  it("runs each runtime mode in the Command Code mode that enforces it", () => {
    const mode = (
      runtimeMode: Parameters<typeof commandCodeSessionModeForPolicy>[0]["runtimeMode"],
      overrides: { approvalPolicy?: unknown; sandboxPolicy?: unknown } = {},
    ) => commandCodeSessionModeForPolicy({ runtimeMode, ...overrides });
    assert.equal(mode("approval-required"), "default");
    assert.equal(mode("auto-accept-edits"), "auto-accept");
    assert.equal(mode("full-access"), "bypass");
    // `dont-ask` denies what needs approval, which is not Onicode's Auto.
    assert.equal(mode("auto"), "default");
    assert.equal(mode("full-access", { approvalPolicy: "on-request" }), "default");
    assert.equal(mode("full-access", { sandboxPolicy: "workspace-write" }), "default");
  });

  it("reads the sign-in state from ACP authenticate", () => {
    assert.deepEqual(commandCodeAuthFromProbe(Result.succeed(undefined), {}), {
      status: "authenticated",
      label: "Command Code account",
    });
    assert.deepEqual(
      commandCodeAuthFromProbe(Result.succeed(undefined), { COMMAND_CODE_API_KEY: "key" }),
      { status: "authenticated", type: "api_key", label: "Command Code API key" },
    );
    const signedOut = new EffectAcpErrors.AcpRequestError({
      code: -32000,
      errorMessage: "Not authenticated. Run `cmd login` in a terminal, then retry.",
    });
    assert.deepEqual(commandCodeAuthFromProbe(Result.fail(signedOut), {}), {
      status: "unauthenticated",
    });
    const crashed = new EffectAcpErrors.AcpRequestError({ code: -32603, errorMessage: "boom" });
    assert.deepEqual(commandCodeAuthFromProbe(Result.fail(crashed), {}), { status: "unknown" });
    assert.deepEqual(commandCodeAuthFromProbe(undefined, {}), { status: "unknown" });
  });

  it("learns models and each model's effort levels from the sessions that ran them", () => {
    const fresh = learnCommandCodeCatalog(EMPTY_COMMAND_CODE_CATALOG, [
      modelOption("deepseek/deepseek-v4.1-flash"),
    ]);
    assert.deepEqual(
      fresh.models.map((model) => model.id),
      ["deepseek/deepseek-v4.1-flash", "claude-sonnet-5-5"],
    );
    assert.equal(fresh.effortByModel.size, 0);

    const deepseek = learnCommandCodeCatalog(fresh, [
      modelOption("deepseek/deepseek-v4.1-flash"),
      effortOption(["default", "off", "high", "max"], "high"),
    ]);
    const claude = learnCommandCodeCatalog(deepseek, [
      modelOption("claude-sonnet-5-5"),
      effortOption(["default", "low", "medium", "high", "xhigh", "max"], "default"),
    ]);
    // A session without the effort option keeps what earlier sessions showed.
    assert.equal(learnCommandCodeCatalog(claude, [modelOption("claude-sonnet-5-5")]), claude);

    const withDefault = { ...claude, defaultModelId: "deepseek/deepseek-v4.1-flash" };
    const models = commandCodeModels(decodeSettings({}), withDefault);
    const effortIds = (slug: string) => {
      const descriptor = models.find((model) => model.slug === slug)?.capabilities
        ?.optionDescriptors?.[0];
      return descriptor?.type === "select"
        ? { id: descriptor.id, role: descriptor.role, choices: descriptor.options.map((o) => o.id) }
        : undefined;
    };
    assert.deepEqual(
      models.map((model) => model.slug),
      ["default", "deepseek/deepseek-v4.1-flash", "claude-sonnet-5-5"],
    );
    assert.deepEqual(effortIds("default"), {
      id: "effort",
      role: "reasoning",
      choices: ["default", "off", "high", "max"],
    });
    assert.deepEqual(effortIds("claude-sonnet-5-5")?.choices, [
      "default",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
  });
});
