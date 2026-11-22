// Free of Sentry imports so the mobile app can share it without a second Sentry SDK copy.

// The query and fragment of a URL inside a JSON string. They can carry tickets, tokens and, in
// the app's own hash routes, a linked prompt.
const URL_QUERY = /([a-z][a-z0-9+.-]*:\/\/[^"\s?#\\]*)[?#][^"\s\\]*/giu;
// Navigation breadcrumbs hold app routes without a scheme, such as `/#/new?prompt=...`.
const ROUTE_QUERY = /("(?:from|to)":"[^"?\\]*)\?[^"\\]*/gu;

/**
 * Prepares a Sentry payload to leave the machine: drops URL queries and fragments, and replaces
 * the user's home directory with `~`. Pass an empty home directory where there is none.
 */
export function scrubSentryPayload<T>(payload: T, homeDirectory: string): T {
  const json = JSON.stringify(payload);
  if (json === undefined) return payload;
  let scrubbed = json.replace(URL_QUERY, "$1").replace(ROUTE_QUERY, "$1");
  if (homeDirectory.length >= 2) {
    // Match the home directory as it appears inside JSON strings, escaped backslashes included.
    scrubbed = scrubbed.replaceAll(JSON.stringify(homeDirectory).slice(1, -1), "~");
  }
  return scrubbed === json ? payload : (JSON.parse(scrubbed) as T);
}
