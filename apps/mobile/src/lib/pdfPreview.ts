import { translatableError } from "@onicode/client-runtime/errors";
import { downloadAttachmentForPreview } from "./attachmentDownload";
import { msg } from "gt-react-native";

export function isPdfPreviewUri(uri: string): boolean {
  try {
    return /\.pdf$/i.test(new URL(uri).pathname);
  } catch {
    return false;
  }
}

export function pdfInitialPage(uri: string): number {
  const page = Number(new URLSearchParams(uri.split("#", 2)[1]).get("page"));
  return Number.isSafeInteger(page) && page > 0 ? page : 1;
}

const PDF_LINK_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);

export function pdfLinkUrl(href: string, sourceUri: string): string | null {
  if (href.trim().length === 0) return null;
  try {
    const url = new URL(href, sourceUri);
    return PDF_LINK_PROTOCOLS.has(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

const withoutFragment = (url: string) => url.split("#", 1)[0];

/**
 * A WebView showing a PDF loads it, its redirects and its page jumps. Once `documentUrl` has
 * loaded, only a tapped top-frame link (iOS `navigationType` "click") opens outside; anything else
 * is blocked, so a page served at a `.pdf` URL cannot open the OS on its own.
 */
export function pdfWebViewNavigation(
  request: { readonly url: string; readonly navigationType: string; readonly isTopFrame: boolean },
  documentUrl: string | null,
): { readonly kind: "load" | "block" } | { readonly kind: "open"; readonly url: string } {
  if (documentUrl === null || withoutFragment(request.url) === withoutFragment(documentUrl)) {
    return { kind: "load" };
  }
  const url =
    request.isTopFrame && request.navigationType === "click"
      ? pdfLinkUrl(request.url, documentUrl)
      : null;
  return url === null ? { kind: "block" } : { kind: "open", url };
}

export async function preparePdfPreview(uri: string, signal: AbortSignal) {
  if (signal.aborted) return null;
  const source = uri.split("#", 1)[0]!;
  const cached = /^(file|content):/.test(source)
    ? { uri: source, dispose: () => {} }
    : await downloadAttachmentForPreview({
        url: source,
        attachment: { name: "preview.pdf", mimeType: "application/pdf" },
        signal,
      });
  if (cached === null) return null;
  try {
    const { File, FileMode } = await import("expo-file-system");
    const handle = new File(cached.uri).open(FileMode.ReadOnly);
    try {
      const header = await handle.readBytes(5);
      if (String.fromCharCode(...header) !== "%PDF-") {
        throw translatableError(
          msg("The server did not return a PDF. Check the connection and try again."),
        );
      }
    } finally {
      handle.close();
    }
    if (signal.aborted) {
      cached.dispose();
      return null;
    }
    return cached;
  } catch (error) {
    cached.dispose();
    if (signal.aborted) return null;
    throw error;
  }
}
