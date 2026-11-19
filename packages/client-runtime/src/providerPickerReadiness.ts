import type { ServerProvider } from "@onicode/contracts";

type ProviderPickerState = Pick<
  ServerProvider,
  "enabled" | "availability" | "status" | "installed" | "auth" | "subProviders" | "modelsRetained"
> & { readonly models: ReadonlyArray<{ readonly subProvider?: string | undefined }> };

/**
 * Disabled instances may retain ready status until reconciliation. A warning still offers
 * the models of sub-providers that report no problem of their own.
 */
export function isProviderPickerHealthy(provider: ProviderPickerState): boolean {
  if (!provider.enabled || provider.availability === "unavailable") return false;
  if (provider.status === "ready") return true;
  if (provider.status !== "warning") return false;
  const usable = new Set(
    provider.subProviders?.flatMap((subProvider) =>
      subProvider.unavailableReason === undefined ? [subProvider.name] : [],
    ),
  );
  if (usable.size === 0) return false;
  return provider.models.some(
    (model) => model.subProvider !== undefined && usable.has(model.subProvider),
  );
}

/** Also true after a check that only failed or ran late: an earlier check's models stay offered. */
export function isProviderPickerReady(provider: ProviderPickerState): boolean {
  if (!provider.installed || provider.auth.status === "unauthenticated") return false;
  if (isProviderPickerHealthy(provider)) return true;
  if (
    !provider.enabled ||
    provider.availability === "unavailable" ||
    provider.subProviders !== undefined
  ) {
    return false;
  }
  if (provider.modelsRetained === true) return true;
  // The sign-in was confirmed, so the warning is about something else, such as a late catalog.
  return (
    provider.status === "warning" &&
    provider.auth.status === "authenticated" &&
    provider.models.length > 0
  );
}
