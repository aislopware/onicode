import { resolveTextScaleVariables } from "./appearancePreferences";
import {
  resolveFontFamilyVariables,
  type MobileFontId,
  type MobileMonoFontId,
} from "./fontFamilies";
import { type MobileThemeAppearance, type MobileThemeMode } from "./mobileTheme";

export interface MobileThemeRuntimeState {
  readonly baseFontSize: number;
  readonly fontId: MobileFontId;
  readonly monoFontId: MobileMonoFontId;
  readonly themeAppearance: MobileThemeAppearance;
  readonly themeMode: MobileThemeMode;
}

export type MobileThemeRuntimeOperation =
  | {
      readonly kind: "update-text-variables";
      readonly themeName: MobileThemeAppearance;
      readonly variables: Readonly<Record<string, string | number>>;
    }
  | {
      readonly kind: "set-appearance-mode";
      readonly appearance: MobileThemeAppearance;
      readonly themeMode: MobileThemeMode;
    };

const UNIWIND_THEME_NAMES: ReadonlyArray<MobileThemeAppearance> = ["light", "dark"];

export function createMobileThemeRuntimeOperations(
  previous: MobileThemeRuntimeState | null,
  next: MobileThemeRuntimeState,
): ReadonlyArray<MobileThemeRuntimeOperation> {
  const operations: MobileThemeRuntimeOperation[] = [];

  if (
    previous === null ||
    previous.baseFontSize !== next.baseFontSize ||
    previous.fontId !== next.fontId ||
    previous.monoFontId !== next.monoFontId
  ) {
    const variables = {
      ...resolveTextScaleVariables(next.baseFontSize),
      ...resolveFontFamilyVariables(next.fontId, next.monoFontId),
    };
    for (const themeName of UNIWIND_THEME_NAMES) {
      operations.push({ kind: "update-text-variables", themeName, variables });
    }
  }

  if (previous === null || previous.themeMode !== next.themeMode) {
    operations.push({
      kind: "set-appearance-mode",
      appearance: next.themeAppearance,
      themeMode: next.themeMode,
    });
  }

  return operations;
}
