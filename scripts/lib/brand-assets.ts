export const BRAND_ASSET_PATHS = {
  developmentIconComposerProject: "assets/dev/app-icon.icon",
  developmentIosIconPng: "assets/dev/blueprint-ios-1024.png",
  developmentUniversalIconPng: "assets/dev/blueprint-universal-1024.png",

  productionIconComposerProject: "assets/prod/app-icon.icon",
  productionIosIconPng: "assets/prod/black-ios-1024.png",
  productionMacIconPng: "assets/prod/black-macos-1024.png",
  productionLinuxIconPng: "assets/prod/black-universal-1024.png",
  productionWindowsIconIco: "assets/prod/oni-black-windows.ico",
  productionSplashLogoPng: "assets/prod/oni-black-splash-logo-180.png",

  developmentDesktopIconPng: "assets/dev/blueprint-macos-1024.png",
  developmentWindowsIconIco: "assets/dev/blueprint-windows.ico",
  developmentSplashLogoPng: "assets/dev/blueprint-splash-logo-180.png",
} as const;

export type WebAssetBrand = "development" | "production";

export interface IconOverride {
  readonly sourceRelativePath: string;
  readonly targetRelativePath: string;
}

const SPLASH_LOGO_SOURCE_PATHS_BY_BRAND = {
  development: BRAND_ASSET_PATHS.developmentSplashLogoPng,
  production: BRAND_ASSET_PATHS.productionSplashLogoPng,
} as const satisfies Record<WebAssetBrand, string>;

/** The renderer's only branded file is the boot splash logo that index.html shows. */
export function resolveWebIconOverrides(
  brand: WebAssetBrand,
  targetDirectory: string,
): ReadonlyArray<IconOverride> {
  return [
    {
      sourceRelativePath: SPLASH_LOGO_SOURCE_PATHS_BY_BRAND[brand],
      targetRelativePath: `${targetDirectory}/splash-logo.png`,
    },
  ];
}

export const DEVELOPMENT_ICON_OVERRIDES = resolveWebIconOverrides("development", "dist/client");

export const DEVELOPMENT_PUBLIC_ICON_OVERRIDES = resolveWebIconOverrides(
  "development",
  "apps/web/public",
);
