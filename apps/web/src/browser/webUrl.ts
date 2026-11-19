export function isWebUrl(url: string): boolean {
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

/** A web link as the system browser reads it, such as `http:localhost:5173`; others unchanged. */
export const canonicalLinkUrl = (url: string): string => (isWebUrl(url) ? new URL(url).href : url);
