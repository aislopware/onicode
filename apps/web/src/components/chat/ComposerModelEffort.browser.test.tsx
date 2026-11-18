import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderOptionSelection,
  type ServerProviderModel,
} from "@onicode/contracts";
import { createModelCapabilities } from "@onicode/shared/model";
import { useState } from "react";
import { expect, it } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import { render } from "../../../test/render";
import { ComposerEffortColumn, describeComposerEffort } from "./ComposerModelEffort";
import type { TraitsSelectionInput } from "./TraitsPicker";

// Shaped like Devin over ACP: its session mode comes first, and its reasoning id is its own.
const models: ReadonlyArray<ServerProviderModel> = [
  {
    slug: "fusion",
    name: "Fusion (Claude Opus 5.5 Medium + SWE-2 High)",
    isCustom: false,
    capabilities: createModelCapabilities({
      optionDescriptors: [
        {
          id: "mode",
          label: "Session Mode",
          type: "select",
          currentValue: "bypass",
          options: [
            { id: "code", label: "Code" },
            { id: "plan", label: "Plan" },
            { id: "bypass", label: "Bypass Permissions" },
          ],
        },
        {
          id: "thought_level",
          label: "Thinking",
          type: "select",
          role: "reasoning",
          currentValue: "medium",
          options: [
            { id: "low", label: "Low" },
            { id: "medium", label: "Medium" },
            { id: "high", label: "High" },
          ],
        },
        {
          id: "speed",
          label: "Speed",
          type: "select",
          currentValue: "standard",
          options: [
            { id: "standard", label: "Standard" },
            { id: "fast", label: "Fast" },
          ],
        },
        { id: "auto_approve", label: "Auto-approve tools", type: "boolean", currentValue: false },
      ],
    }),
  },
];

function traitsInput(
  modelOptions: ReadonlyArray<ProviderOptionSelection>,
  onModelOptionsChange: (next: ReadonlyArray<ProviderOptionSelection> | undefined) => void,
): TraitsSelectionInput {
  return {
    provider: ProviderDriverKind.make("acpRegistry"),
    instanceId: ProviderInstanceId.make("devin"),
    models,
    model: "fusion",
    modelOptions,
    onModelOptionsChange,
  };
}

function Column() {
  const [options, setOptions] = useState<ReadonlyArray<ProviderOptionSelection>>([]);
  const traits = traitsInput(options, (next) => setOptions(next ?? []));
  return (
    <>
      <div data-model-picker-effort>
        <ComposerEffortColumn traits={traits} />
      </div>
      <p role="status">{describeComposerEffort(traits, "bars", "sm").suffix}</p>
    </>
  );
}

it("draws an agent's reasoning as bars and keeps its mode out of the levels", async () => {
  await using screen = await render(<Column />, { router: false }); // no routes involved
  const status = screen.getByRole("status");
  expect(status.element().querySelector("[data-composer-effort-glyph]")).not.toBeNull();
  await expect.element(status).toHaveTextContent("");

  // Three levels on the slider, so the session mode's three choices are not among them.
  const levels = screen.getByRole("slider", { name: "Thinking" });
  await expect.element(levels).toHaveAttribute("max", "2");
  await expect.element(levels).toHaveAttribute("aria-valuetext", "Medium");

  // Left and Right move the level. Up is the popup's way back to the list, so it changes nothing.
  await userEvent.tab();
  await expect.element(levels).toHaveFocus();
  await userEvent.keyboard("{ArrowRight}");
  await expect.element(levels).toHaveAttribute("aria-valuetext", "High");
  await userEvent.keyboard("{ArrowLeft}{ArrowUp}");
  await expect.element(levels).toHaveAttribute("aria-valuetext", "Medium");
  // Down leaves the slider for the options, whose choices sit in their rows.
  await userEvent.keyboard("{ArrowDown}");
  await expect.element(screen.getByRole("radio", { name: "Bypass Permissions" })).toHaveFocus();
  await userEvent.keyboard("{ArrowDown}");
  const standard = screen.getByRole("radio", { name: "Standard" });
  await expect.element(standard).toHaveFocus();
  // Left and Right walk one option's choices; Enter picks.
  await userEvent.keyboard("{ArrowRight}{Enter}");
  await expect.element(screen.getByRole("radio", { name: "Fast" })).toHaveFocus();
  await expect
    .element(screen.getByRole("radio", { name: "Fast" }))
    .toHaveAttribute("aria-checked", "true");
  await userEvent.keyboard("{ArrowUp}");
  await expect.element(screen.getByRole("radio", { name: "Bypass Permissions" })).toHaveFocus();

  await userEvent.click(screen.getByText("Auto-approve tools"));
  await expect
    .element(screen.getByRole("switch", { name: "Auto-approve tools" }))
    .toHaveAttribute("aria-checked", "true");

  await userEvent.click(screen.getByRole("radio", { name: "Plan" }));
  await expect
    .element(screen.getByRole("radio", { name: "Plan" }))
    .toHaveAttribute("aria-checked", "true");
  await expect.element(page.getByRole("menuitemradio")).not.toBeInTheDocument();
});
