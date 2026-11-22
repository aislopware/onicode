import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import type { ModelSelection, ProjectId, ProviderInstanceId } from "@onicode/contracts";
import {
  isProviderAvailable,
  isProviderTextGenerationCapable,
  resolveProjectProviderInstanceEnabled,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import { exhaustedUsageWindow } from "@onicode/shared/usageLimits";
import { fallbackTextGenerationModelSelection } from "@onicode/shared/serverSettings";
import * as ServerSettings from "../serverSettings.ts";
import type { ProviderTextGeneration } from "@onicode/provider-core/server/textGeneration";
import { TextGenerationError } from "@onicode/contracts";

import * as ProviderInstallationGate from "../provider/ProviderInstallationGate.ts";
import * as ProviderInstanceRegistry from "../provider/ProviderInstanceRegistry.ts";
import type { ProviderInstance } from "@onicode/provider-core/server/driver";
import * as SourceControlProviderRegistry from "../sourceControl/SourceControlProviderRegistry.ts";
import * as ThreadTitleLinks from "./ThreadTitleLinks.ts";
import * as TextGenerators from "./TextGenerators.ts";
import * as TextGenerationOperations from "@onicode/provider-core/server/textGenerationOperations";

export type {
  BranchNameGenerationInput,
  BranchNameGenerationResult,
  CommitMessageGenerationInput,
  CommitMessageGenerationResult,
  PrContentGenerationInput,
  PrContentGenerationResult,
  ReviewGuideGenerationInput,
  ReviewGuideGenerationResult,
  ThreadTitleGenerationInput,
  ThreadTitleGenerationResult,
  TranscriptionPostProcessingInput,
  TranscriptionPostProcessingResult,
} from "@onicode/provider-core/server/textGeneration";

export class TextGeneration extends Context.Service<TextGeneration, ProviderTextGeneration>()(
  "onicode/textGeneration/TextGeneration",
) {}

type TextGenerationOp =
  | "generateCommitMessage"
  | "generatePrContent"
  | "generateBranchName"
  | "generateThreadTitle"
  | "generateReviewGuide"
  | "generateTranscriptionPostProcessing";

const resolveInstance = (
  registry: ProviderInstanceRegistry.ProviderInstanceRegistry["Service"],
  operation: TextGenerationOp,
  instanceId: ProviderInstanceId,
): Effect.Effect<ProviderInstance["textGeneration"], TextGenerationError> =>
  registry.getInstance(instanceId).pipe(
    Effect.flatMap((instance) =>
      instance
        ? Effect.succeed(instance.textGeneration)
        : Effect.fail(
            new TextGenerationError({
              operation,
              detail: `No provider instance registered for id '${instanceId}'.`,
            }),
          ),
    ),
  );

// Review guides investigate the checkout read-only; the other drivers reject that request.
const INVESTIGATING_DRIVERS = new Set(["codex", "claudeAgent"]);

const CANNOT_GENERATE: Record<TextGenerationOp, string> = {
  generateCommitMessage: "write commit messages",
  generatePrContent: "write pull requests",
  generateBranchName: "name branches",
  generateThreadTitle: "title threads",
  generateReviewGuide: "write review guides",
  generateTranscriptionPostProcessing: "clean up transcripts",
};

const TEXT_MODEL_HINT = "another text generation model in Settings → Threads";
const WRITER_HINT = `Pick another source control writer model in Settings → Source control & issues, or ${TEXT_MODEL_HINT}.`;
const PICK_ANOTHER_MODEL: Record<TextGenerationOp, string> = {
  generateCommitMessage: WRITER_HINT,
  generatePrContent: WRITER_HINT,
  generateBranchName: WRITER_HINT,
  generateThreadTitle: `Pick ${TEXT_MODEL_HINT}.`,
  generateReviewGuide: "Pick another review guide model in Settings → Source control & issues.",
  generateTranscriptionPostProcessing: `Pick another cleanup model in Settings → Voice, or ${TEXT_MODEL_HINT}.`,
};

/** @public */
export const make = Effect.gen(function* () {
  const registry = yield* ProviderInstanceRegistry.ProviderInstanceRegistry;
  const sourceControl = yield* SourceControlProviderRegistry.SourceControlProviderRegistry;
  const serverSettings = yield* ServerSettings.ServerSettingsService;
  const installationGate = yield* ProviderInstallationGate.ProviderInstallationGate;
  const textGenerators = yield* TextGenerators.TextGenerators;
  const generate = Effect.fn("TextGeneration.generate")(function* <A>(
    operation: TextGenerationOp,
    selection: ModelSelection,
    projectId: ProjectId | null | undefined,
    invoke: (
      service: ProviderTextGeneration,
      modelSelection: ModelSelection,
    ) => Effect.Effect<A, TextGenerationError>,
  ) {
    const textGenerator = yield* textGenerators.runner(selection.instanceId);
    // A review guide reaches a text generator only through a fallback; it needs an agent instead.
    const textGeneratorId = selection.instanceId;
    const textGeneratorCannotRun =
      textGenerator !== undefined && operation === "generateReviewGuide";
    if (textGenerator !== undefined && !textGeneratorCannotRun) {
      return yield* invoke(
        TextGenerationOperations.fromRunner("TextGenerator", textGenerator),
        selection,
      );
    }
    const snapshots = yield* registry.listInstances.pipe(
      Effect.flatMap((instances) =>
        Effect.forEach(instances, (instance) => instance.snapshot.getSnapshot),
      ),
    );
    const now = DateTime.toEpochMillis(yield* DateTime.now);
    const settings = yield* serverSettings.getSettings.pipe(
      Effect.mapError(
        (cause) =>
          new TextGenerationError({
            operation,
            detail: "Could not read provider settings.",
            cause,
          }),
      ),
    );
    const selected = snapshots.find((provider) => provider.instanceId === selection.instanceId);
    const off =
      selected !== undefined &&
      (!selected.enabled ||
        !resolveProjectProviderInstanceEnabled(settings, projectId ?? null, selected.instanceId));
    // A provider still probing, or signed out, can report no text generation until it is ready.
    const cannotGenerateText =
      selected !== undefined &&
      ((selected.status === "ready" && !isProviderTextGenerationCapable(selected)) ||
        selected.models.find((model) => model.slug === selection.model)?.supportsTextGeneration ===
          false ||
        (operation === "generateReviewGuide" && !INVESTIGATING_DRIVERS.has(selected.driver)));
    // Only a failed probe counts: a pending one is also not installed yet.
    const unusable =
      textGeneratorCannotRun ||
      off ||
      cannotGenerateText ||
      (selected !== undefined && !isProviderTextGenerationCapable(selected)) ||
      (selected !== undefined &&
        selected.status === "error" &&
        (!selected.installed || selected.auth.status === "unauthenticated"));
    if (unusable || exhaustedUsageWindow(selected, now, selection.model)) {
      const rejected = selection;
      const fallbackWhere = (sameInstance: boolean) =>
        fallbackTextGenerationModelSelection(
          settings.providerInstances,
          (instanceId, driver, model) => {
            const provider = snapshots.find((candidate) => candidate.instanceId === instanceId);
            return (
              (sameInstance ? model !== rejected.model : instanceId !== rejected.instanceId) &&
              provider?.enabled === true &&
              resolveProjectProviderInstanceEnabled(settings, projectId ?? null, instanceId) &&
              provider.installed &&
              provider.auth.status !== "unauthenticated" &&
              provider.status !== "error" &&
              isProviderAvailable(provider) &&
              isProviderTextGenerationCapable(provider) &&
              provider.models.find((candidate) => candidate.slug === model)
                ?.supportsTextGeneration !== false &&
              (operation !== "generateReviewGuide" || INVESTIGATING_DRIVERS.has(driver)) &&
              !exhaustedUsageWindow(provider, now, model)
            );
          },
          { defaultModelSelection: settings.defaultModelSelection },
        );
      // Another instance first; the rejected instance's other models only when none qualifies.
      const fallback = fallbackWhere(false) ?? fallbackWhere(true);
      // A provider the user turned off is never run, even when nothing else can take over.
      if (fallback === null && off) {
        return yield* new TextGenerationError({
          operation,
          detail:
            "The selected provider is off and no other provider can take over. Turn one on in Settings.",
        });
      }
      // A failed probe or a full usage window can clear by run time; this refusal cannot.
      if (fallback === null && cannotGenerateText) {
        return yield* new TextGenerationError({
          operation,
          detail: `${selected?.displayName ?? selection.instanceId} cannot ${CANNOT_GENERATE[operation]} with ${selection.model} and no other provider can take over. ${PICK_ANOTHER_MODEL[operation]}`,
        });
      }
      selection = fallback ?? selection;
    }
    if (textGeneratorCannotRun && selection.instanceId === textGeneratorId) {
      return yield* new TextGenerationError({
        operation,
        detail:
          "Review guides need a coding agent that can read the checkout. None is ready. Pick Codex or Claude as the review guide model in Settings → Source control & issues.",
      });
    }
    return yield* installationGate.execute(
      selection.instanceId,
      resolveInstance(registry, operation, selection.instanceId).pipe(
        Effect.flatMap((service) => invoke(service, selection)),
      ),
    );
  });
  return TextGeneration.of({
    generateCommitMessage: Effect.fn("TextGeneration.generateCommitMessage")(function (input) {
      return generate(
        "generateCommitMessage",
        input.modelSelection,
        input.projectId,
        (service, modelSelection) => service.generateCommitMessage({ ...input, modelSelection }),
      );
    }),
    generatePrContent: Effect.fn("TextGeneration.generatePrContent")(function (input) {
      return generate(
        "generatePrContent",
        input.modelSelection,
        input.projectId,
        (service, modelSelection) => service.generatePrContent({ ...input, modelSelection }),
      );
    }),
    generateBranchName: Effect.fn("TextGeneration.generateBranchName")(function (input) {
      return generate(
        "generateBranchName",
        input.modelSelection,
        input.projectId,
        (service, modelSelection) => service.generateBranchName({ ...input, modelSelection }),
      );
    }),
    generateReviewGuide: Effect.fn("TextGeneration.generateReviewGuide")(function (input) {
      return generate(
        "generateReviewGuide",
        input.modelSelection,
        input.projectId,
        (service, modelSelection) =>
          service
            .generateReviewGuide({ ...input, modelSelection })
            .pipe(Effect.map((guide) => ({ ...guide, modelSelection }))),
      );
    }),
    generateTranscriptionPostProcessing: Effect.fn(
      "TextGeneration.generateTranscriptionPostProcessing",
    )(function (input) {
      return generate(
        "generateTranscriptionPostProcessing",
        input.modelSelection,
        null,
        (service, modelSelection) =>
          service.generateTranscriptionPostProcessing({ ...input, modelSelection }),
      );
    }),
    generateThreadTitle: Effect.fn("TextGeneration.generateThreadTitle")(function (input) {
      return generate(
        "generateThreadTitle",
        input.modelSelection,
        input.projectId,
        (textGeneration, modelSelection) =>
          Effect.gen(function* () {
            const linkedContext =
              input.linkedContext ??
              (yield* ThreadTitleLinks.resolveThreadTitleLinks(input).pipe(
                Effect.provideService(
                  SourceControlProviderRegistry.SourceControlProviderRegistry,
                  sourceControl,
                ),
              ));
            return yield* textGeneration.generateThreadTitle({
              ...input,
              linkedContext,
              modelSelection,
            });
          }),
      );
    }),
  });
});

export const layer = Layer.effect(TextGeneration, make).pipe(
  Layer.provideMerge(TextGenerators.layer),
);
