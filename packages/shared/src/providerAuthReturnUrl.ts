/**
 * A browser sign-in may only send the user back to the desktop app itself
 * (`onicode://app/` or `onicode-dev://app/`), which brings its window forward.
 */
export function providerAuthReturnUrl(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return (url.protocol === "onicode:" || url.protocol === "onicode-dev:") &&
      url.host === "app" &&
      url.href === `${url.protocol}//app/`
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}
