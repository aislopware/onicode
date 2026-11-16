import defaultThemeVariables from "../../generated-uniwind-default-theme-variables.json";
import {
  flattenThemeColor,
  themeColorWithAlpha,
  themeColorWithContrast,
  type MobileThemeAppearance,
  type MobileThemeVariables,
} from "./mobileTheme";

const defaults = defaultThemeVariables as Readonly<
  Record<MobileThemeAppearance, MobileThemeVariables>
>;

const CARD_TEXT_ROLES = [
  "--color-foreground-secondary",
  "--color-foreground-muted",
  "--color-placeholder",
  "--color-primary-text",
] as const;

// A selected row in a sidebar sits on the drawer, so it must stay visible there with its labels.
function withVisibleSelection(variables: MobileThemeVariables): MobileThemeVariables {
  const selected = themeColorWithContrast(
    flattenThemeColor(variables["--color-thread-selected"], variables["--color-drawer"]),
    flattenThemeColor(variables["--color-drawer"], variables["--color-screen"]),
    1.2,
  );
  return {
    ...variables,
    "--color-thread-selected": selected,
    "--color-thread-selected-foreground": themeColorWithContrast(
      variables["--color-thread-selected-foreground"],
      selected,
    ),
    "--color-thread-selected-foreground-muted": themeColorWithContrast(
      variables["--color-thread-selected-foreground-muted"],
      selected,
    ),
  };
}

export function getMobileThemeRuntimeVariables(
  appearance: MobileThemeAppearance,
  platform: string,
): MobileThemeVariables {
  return withVisibleSelection(platformThemeVariables(appearance, platform));
}

function platformThemeVariables(
  appearance: MobileThemeAppearance,
  platform: string,
): MobileThemeVariables {
  const variables = defaults[appearance];
  const frame = themeColorWithAlpha(variables["--color-row-hover"], 1);
  if (platform === "ios" && appearance === "light") {
    return {
      ...variables,
      "--color-header": frame,
      "--color-header-foreground": variables["--color-drawer-foreground"],
      "--color-drawer": frame,
      "--color-drawer-foreground-muted": variables["--color-foreground-muted"],
    };
  }
  if (platform !== "android") return variables;

  // Android paints the drawer as the page inside a lighter frame, so dark cards lift off both.
  const screen = variables["--color-screen"];
  const groupedCard =
    appearance === "dark"
      ? themeColorWithContrast(
          variables["--color-grouped-card"],
          [screen, flattenThemeColor(frame, screen)],
          1.2,
        )
      : variables["--color-grouped-card"];
  const readableOnCard = (role: (typeof CARD_TEXT_ROLES)[number]) =>
    [role, themeColorWithContrast(variables[role], groupedCard)] as const;
  return {
    ...variables,
    ...Object.fromEntries(CARD_TEXT_ROLES.map(readableOnCard)),
    "--color-header": frame,
    "--color-header-foreground": variables["--color-drawer-foreground"],
    "--color-drawer": screen,
    "--color-grouped-card": groupedCard,
  };
}
