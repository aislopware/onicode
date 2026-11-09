/** "Dev" on the development client; released builds carry no stage label. */
export function resolveMobileStageLabel(appVariant: unknown): "Dev" | null {
  return appVariant === "development" ? "Dev" : null;
}
