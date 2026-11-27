import { IssueTrackerKind } from "@onicode/contracts";

export const JIRA_KIND = IssueTrackerKind.make("jira");
export const JIRA_HOST = "atlassian.net";
const JIRA_SITE = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.atlassian\.net$/;

/**
 * A Jira Cloud issue link, keeping the site as the workspace: `/browse/<PROJECT>-<n>`, a
 * project's `…/issues/<PROJECT>-<n>`, or a board or search naming it as `selectedIssue`.
 */
export function parseJiraIssueUrl(url: URL) {
  const site = url.hostname.toLowerCase();
  if (url.protocol !== "https:" || url.port !== "" || !JIRA_SITE.test(site)) return null;
  const segments = url.pathname.split("/").filter((segment) => segment !== "");
  // Confluence shares the site under /wiki.
  if (segments[0] === "wiki") return null;
  const named =
    segments[0] === "browse"
      ? segments.length === 2
        ? segments[1]
        : undefined
      : segments.at(-2) === "issues"
        ? segments.at(-1)
        : undefined;
  const key = named ?? url.searchParams.get("selectedIssue") ?? "";
  const scope = /^([A-Z][A-Z0-9_]*)-[1-9][0-9]*$/.exec(key)?.[1];
  return scope === undefined ? null : { host: JIRA_HOST, workspace: site, scope, key };
}
