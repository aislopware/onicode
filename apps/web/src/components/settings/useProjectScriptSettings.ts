import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import {
  EnvironmentAuthorizationError,
  requiredScopesForServerSettingsPatch,
} from "@onicode/contracts";
import { readEnvironmentScope } from "../../state/session";
import {
  isAtomCommandInterrupted,
  mapAtomCommandResult,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@onicode/client-runtime/state/runtime";
import {
  type EnvironmentId,
  type ProjectId,
  type ProjectScript,
  type ResolvedKeybindingsConfig,
  type ServerSettings,
} from "@onicode/contracts";
import { resolveProjectScripts } from "@onicode/shared/projectScripts";
import {
  clearProjectSettingsOverrides,
  projectOverridesPatch,
} from "@onicode/shared/projectSettings";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { msg } from "gt-react";
import { useRef, useState } from "react";

import {
  decodeProjectScriptKeybindingRule,
  keybindingValueForCommand,
} from "../../lib/projectScriptKeybindings";
import {
  buildProjectScript,
  commandForProjectScript,
  nextProjectScriptId,
  releaseClaimedRoles,
} from "../../projectScripts";
import { useProjects } from "../../state/entities";
import { serverEnvironment } from "../../state/server";
import { useAtomCommand } from "../../state/use-atom-command";
import type { NewProjectScriptInput } from "../projectScriptEditor";
import { m } from "../../i18n/messages";
import { toastManager } from "../ui/toast";

function reportScriptFailure(result: AtomCommandResult<unknown, unknown>) {
  if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
    const error = squashAtomCommandFailure(result);
    toastManager.add({
      type: "error",
      title: m(msg("Failed to save project actions")),
      description:
        error instanceof Error
          ? (thrownErrorMessage(error, m) ?? error.message)
          : m(msg("An error occurred.")),
    });
  }
  return mapAtomCommandResult(result, () => undefined);
}

interface ProjectScriptTarget {
  environmentId: EnvironmentId;
  settings: ServerSettings;
  keybindings: ResolvedKeybindingsConfig;
  project?: { id: ProjectId; scripts: readonly ProjectScript[] };
  perKeyOverrides?: boolean;
}

function planWrite(
  target: ProjectScriptTarget,
  transform: (current: readonly ProjectScript[]) => readonly ProjectScript[] | null,
) {
  const { settings, project, perKeyOverrides } = target;
  const current = project
    ? resolveProjectScripts(settings, project)
    : settings.defaultProjectScripts;
  const nextScripts = transform(current);
  const patch = project
    ? projectOverridesPatch(
        [
          {
            projectId: project.id,
            keys: ["defaultProjectScripts"],
            next:
              nextScripts === null
                ? clearProjectSettingsOverrides(settings, project.id, ["defaultProjectScripts"])
                : {
                    ...settings.projectSettingsOverrides[project.id],
                    defaultProjectScripts: nextScripts,
                  },
          },
        ],
        perKeyOverrides === true,
      )
    : { defaultProjectScripts: nextScripts ?? [] };
  return { target, current, nextScripts, patch };
}

export function useProjectScriptSettings(targets: readonly ProjectScriptTarget[]) {
  const projects = useProjects();
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const updateSettings = useAtomCommand(serverEnvironment.updateSettings, "project actions update");
  const upsertKeybinding = useAtomCommand(
    serverEnvironment.upsertKeybinding,
    "action shortcut update",
  );
  const removeKeybinding = useAtomCommand(
    serverEnvironment.removeKeybinding,
    "action shortcut removal",
  );

  async function persist(
    transform: (current: readonly ProjectScript[]) => readonly ProjectScript[] | null,
    scriptId?: string,
    keybinding?: string | null,
  ): Promise<AtomCommandResult<void, unknown>> {
    if (savingRef.current || targets.length === 0) {
      const message = m(msg("No available machine, or another action change is saving."));
      toastManager.add({
        type: "error",
        title: m(msg("Actions not saved")),
        description: message,
      });
      return AsyncResult.failure(Cause.fail(new Error(message)));
    }
    const writes = targets.map((target) => planWrite(target, transform));
    // Every target is checked before the first write, so a denial leaves none half saved.
    const missing = writes.flatMap(({ target, patch }) =>
      requiredScopesForServerSettingsPatch(patch).filter(
        (scope) => !readEnvironmentScope(target.environmentId, scope),
      ),
    )[0];
    if (missing !== undefined) {
      return reportScriptFailure(
        AsyncResult.failure(
          Cause.fail(
            new EnvironmentAuthorizationError({
              requiredScope: missing,
              message: m(msg("This connection cannot change project actions.")),
            }),
          ),
        ),
      );
    }
    savingRef.current = true;
    setSaving(true);
    return runWrites(writes, scriptId, keybinding).finally(() => {
      savingRef.current = false;
      setSaving(false);
    });
  }

  async function runWrites(
    writes: ReadonlyArray<ReturnType<typeof planWrite>>,
    scriptId: string | undefined,
    keybinding: string | null | undefined,
  ): Promise<AtomCommandResult<void, unknown>> {
    for (const { target, current, nextScripts, patch } of writes) {
      const { environmentId, settings, keybindings, project } = target;
      const effectiveScripts = nextScripts ?? settings.defaultProjectScripts;
      const result = await updateSettings({ environmentId, input: { patch } });
      if (result._tag === "Failure") return reportScriptFailure(result);
      const changedIds = scriptId
        ? [scriptId]
        : current
            .filter((script) => !effectiveScripts.some((next) => next.id === script.id))
            .map((script) => script.id);
      for (const id of changedIds) {
        if (keybinding === undefined && effectiveScripts.some((script) => script.id === id)) {
          continue;
        }
        const command = commandForProjectScript(id);
        const previousValue = keybindingValueForCommand(keybindings, command);
        const previous = previousValue
          ? decodeProjectScriptKeybindingRule({ keybinding: previousValue, command })
          : null;
        const next = decodeProjectScriptKeybindingRule({ keybinding, command });
        const retainedElsewhere =
          !nextScripts?.some((script) => script.id === id) &&
          ((project && settings.defaultProjectScripts.some((script) => script.id === id)) ||
            Object.entries(settings.projectSettingsOverrides).some(
              ([projectId, entry]) =>
                projectId !== project?.id &&
                entry.defaultProjectScripts?.some((script) => script.id === id),
            ) ||
            projects.some(
              (other) =>
                other.environmentId === environmentId &&
                other.id !== project?.id &&
                (project ? resolveProjectScripts(settings, other) : other.scripts).some(
                  (script) => script.id === id,
                ),
            ));
        const bindingResult = next
          ? await upsertKeybinding({
              environmentId,
              input: previous && previous.key !== next.key ? { ...next, replace: previous } : next,
            })
          : previous && !retainedElsewhere
            ? await removeKeybinding({ environmentId, input: previous })
            : null;
        if (bindingResult?._tag === "Failure") return reportScriptFailure(bindingResult);
      }
    }
    return AsyncResult.success(undefined);
  }

  function submit(scriptId: string | null, input: NewProjectScriptInput) {
    const existingIds = [
      ...projects.flatMap((project) => project.scripts.map((script) => script.id)),
      ...targets.flatMap(({ settings, project }) =>
        [
          ...settings.defaultProjectScripts,
          ...Object.values(settings.projectSettingsOverrides).flatMap(
            (entry) => entry.defaultProjectScripts ?? [],
          ),
          ...(project?.scripts ?? []),
        ].map((script) => script.id),
      ),
    ];
    const id = scriptId ?? nextProjectScriptId(input.name, existingIds);
    const next = buildProjectScript(id, input);
    return persist(
      (current) => {
        const updated = current.map((script) =>
          script.id === id ? next : releaseClaimedRoles(script, input),
        );
        return scriptId === null ? [...updated, next] : updated;
      },
      id,
      input.keybinding,
    );
  }

  return { saving, persist, submit };
}
