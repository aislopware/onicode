import { sourceControlClients, type ReferenceAutolink } from "./sourceControlClients.ts";

/**
 * Finds the references the change request's host links in plain text, resolved against the change
 * request's repository. Null when the host links none.
 */
export function changeRequestReferenceAutolinker(
  changeRequest: { readonly provider: string; readonly url: string } | null | undefined,
): ((text: string) => ReadonlyArray<ReferenceAutolink>) | null {
  if (!changeRequest) return null;
  const find = sourceControlClients.get(changeRequest.provider).referenceAutolinks;
  const repositoryUrl = sourceControlClients.parseChangeRequestUrl(changeRequest.url)?.link
    .repositoryUrl;
  return find === undefined || repositoryUrl === undefined
    ? null
    : (text) => find(text, repositoryUrl);
}

/**
 * Forges link uploads root-relative, as GitCafe's `/api/attachments/<id>`. In a change request
 * body that always means the forge's origin, never a path on the machine rendering it.
 */
export function resolveChangeRequestMediaUrl(url: string, changeRequestUrl: string): string {
  if (!/^\/(?![/\\])/u.test(url)) return url;
  try {
    return new URL(url, changeRequestUrl).href;
  } catch {
    return url;
  }
}

const FILE_BLOB_ROUTES: ReadonlyMap<string, string> = new Map([
  ["github", "blob"],
  ["gitlab", "-/blob"],
]);

/**
 * Where the forge shows the change request's files at its head, for path links in a body rendered
 * without the repository's checkout. Null for other forges or without a head commit.
 */
export function changeRequestFileBase(changeRequest: {
  readonly provider: string;
  readonly url: string;
  readonly headSha?: string | undefined;
}): string | null {
  const route = FILE_BLOB_ROUTES.get(changeRequest.provider);
  if (route === undefined || !changeRequest.headSha) return null;
  const repositoryUrl = sourceControlClients.parseChangeRequestUrl(changeRequest.url)?.link
    .repositoryUrl;
  return repositoryUrl === undefined
    ? null
    : `${repositoryUrl}/${route}/${encodeURIComponent(changeRequest.headSha)}/`;
}

/**
 * A path-relative href (no scheme, `/`, `#` or `?`) resolved to a file under `fileBase`, else null,
 * also when `..` climbs out of it.
 */
export function resolveChangeRequestFileHref(
  href: string,
  fileBase: string | null | undefined,
): string | null {
  if (!fileBase || href === "" || /^(?:[a-z][a-z\d+.-]*:|[/\\#?])/iu.test(href)) return null;
  try {
    const base = new URL(fileBase);
    const url = new URL(href, base);
    return url.origin === base.origin &&
      url.pathname.length > base.pathname.length &&
      url.pathname.startsWith(base.pathname)
      ? url.href
      : null;
  } catch {
    return null;
  }
}
