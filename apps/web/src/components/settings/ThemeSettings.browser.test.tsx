import { expect, it } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import {
  ONICODE_CODE_DARK_THEME_COLORS,
  ONICODE_CODE_LIGHT_THEME_COLORS,
  type ThemeDefinition,
} from "@onicode/shared/themePalettes";

import { render } from "../../../test/render";
import { AppRouteModal } from "../AppRouteModal";
import { ThemeLibrary } from "./ThemeSettings";

const collection = { id: "dusk", label: "Dusk" };
const theme = (id: string, appearance: "light" | "dark"): ThemeDefinition => ({
  id,
  label: id,
  appearance,
  colors: appearance === "light" ? ONICODE_CODE_LIGHT_THEME_COLORS : ONICODE_CODE_DARK_THEME_COLORS,
  collection,
});

it("lets Esc close Settings from a theme mode that has no variants to open", async () => {
  await using _screen = await render(
    <AppRouteModal label="Settings">
      <ThemeLibrary
        theme="system"
        setTheme={() => true}
        appearanceMode="system"
        setAppearanceMode={() => true}
        customThemes={[
          theme("Dusk Light", "light"),
          theme("Dusk", "dark"),
          theme("Dusk Deep", "dark"),
        ]}
        initialAppearance="light"
        refreshTheme={() => {}}
        isImportOpen={false}
        onImportOpenChange={() => {}}
        themeHalves={null}
        setThemeHalf={() => true}
      />
    </AppRouteModal>,
    { router: "/settings/appearance" },
  );
  const settings = page.getByRole("dialog", { name: "Settings" });
  const single = page.getByRole("button", { name: /^Use light variant, currently/ });
  await expect.element(single).toBeInTheDocument();
  (single.element() as HTMLElement).focus();

  // The first Esc closes the tooltip focus opened.
  await userEvent.keyboard("{Escape}");
  await expect.element(settings).toHaveAttribute("data-open");

  await userEvent.keyboard("{Escape}");
  await expect.element(settings).not.toBeInTheDocument();
});
