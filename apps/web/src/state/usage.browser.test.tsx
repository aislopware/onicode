import { UsageDay, type UsageSummaryInput } from "@onicode/contracts";
import { getTranslationsSnapshot, GTProvider } from "gt-react";
import { expect, it, vi } from "vite-plus/test";
import { page } from "vite-plus/test/browser";

import { render } from "../../test/render";
import { initializeI18n } from "../i18n/initializeI18n";

vi.mock(import("./presentation"), async (original) => {
  const { Atom } = await import("effect/reactivity");
  const actual = await original();
  const presentation = {
    entry: { target: { label: "workstation" } },
    connection: { phase: "disconnected" },
  };
  return {
    ...actual,
    environmentPresentations: {
      ...actual.environmentPresentations,
      presentationsAtom: Atom.make(new Map([["workstation", presentation]])) as never,
    },
  };
});
vi.mock(import("./session"), async (original) => {
  const { AsyncResult, Atom } = await import("effect/reactivity");
  const actual = await original();
  const session = Atom.make(AsyncResult.success(null));
  return {
    ...actual,
    environmentSession: {
      ...actual.environmentSession,
      sessionStateAtom: (() => session) as never,
    },
  };
});

import { useUsage } from "./usage";

const WINDOW: UsageSummaryInput = {
  sinceDay: UsageDay.make("2026-11-01"),
  untilDay: UsageDay.make("2026-11-28"),
  timeZone: "UTC",
};

function EnvironmentErrors() {
  const { environments } = useUsage(WINDOW);
  return environments.map((environment) => (
    <p key={environment.environmentId}>{environment.error}</p>
  ));
}

it("resolves environment errors again when the catalog for the same locale arrives", async () => {
  // A fresh GT cache, so a German catalog an earlier test loaded cannot answer before this one's.
  initializeI18n(() => {});
  using _locale = {
    [Symbol.dispose]() {
      // GTProvider leaves its locale in a cookie; this sets it back to English.
      initializeI18n(() => {});
    },
  };
  // The runtime publishes the locale before its catalog has loaded.
  await using screen = await render(
    <GTProvider locale="de" translations={{}}>
      <EnvironmentErrors />
    </GTProvider>,
  );
  await expect.element(page.getByText("This environment is not connected.")).toBeVisible();

  const translations = await getTranslationsSnapshot("de");
  await screen.rerender(
    <GTProvider locale="de" translations={translations}>
      <EnvironmentErrors />
    </GTProvider>,
  );

  await expect.element(page.getByText("Diese Umgebung ist nicht verbunden.")).toBeVisible();
});
