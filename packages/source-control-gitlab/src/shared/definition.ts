import { SourceControlProviderKind } from "@onicode/contracts";
import {
  buildCommitUrl,
  changeRequestUrlBelow,
  defineSourceControl,
  isChangeRequestInProjectRepository,
  isChangeRequestOnProjectHost,
  parseChangeRequestPath,
  parseIssuePath,
  parseCommitPath,
  repositoryUrlFromPagePath,
  projectChangeRequestHost,
  remoteWebOrigin,
} from "@onicode/source-control-core/shared/definition";
import { icon } from "./icon.ts";
import { gitLabReferenceAutolinks } from "./referenceAutolinks.ts";

const CHECKOUT_COMMAND = /^glab\s+mr\s+checkout\s+(.+)$/i;
const CHANGE_REQUEST_REFERENCE =
  /^https?:\/\/[^/?#\s]+\/[^/?#\s]+(?:\/[^/?#\s]+)+\/-\/merge_requests\/(\d+)(?:[/?#].*)?$/i;

// Self-managed GitLab runs on any hostname and nests projects in groups.
const CHANGE_REQUEST_PATH = /^\/([^/]+(?:\/[^/]+)+)\/-\/merge_requests\/(\d+)(?:\/|$)/u;
const COMMIT_PATH = /^\/([^/]+(?:\/[^/]+)+)\/-\/commit\/([0-9a-f]+)\/?$/iu;
// An incident is an issue that GitLab also serves at `/-/issues/incident/<iid>`. No project is
// below `/groups/`: a work item there is the group's own, such as an epic.
const ISSUE_PATH =
  /^\/(?!groups\/)([^/]+(?:\/[^/]+)+)\/-\/(?:issues(?:\/incident)?|work_items)\/(\d+)(?:\/|$)/u;
// Every GitLab page inside a project sits below its first `/-/`.
const REPOSITORY_PAGE_PATH = /^\/([^/]+(?:\/[^/]+)+?)\/-\//u;

const KIND = SourceControlProviderKind.make("gitlab");

// A group/project path of GitLab's path characters, not led by the host as in `gitlab.com/g/p`.
function isProjectPath(repository: string, host: string): boolean {
  const segments = repository.split("/");
  const first = segments[0]?.toLowerCase();
  return (
    segments.length >= 2 &&
    segments.every((segment) => /^[\w.-]+$/u.test(segment) && !/^\.\.?$/u.test(segment)) &&
    first !== host.toLowerCase() &&
    first !== host.toLowerCase().replace(/:\d+$/u, "")
  );
}

// GitLab.com also serves SSH on port 443 from this host but names every project on `gitlab.com`.
const ALT_SSH_HOST_KEY = /^altssh\.gitlab\.com\/(.+)$/iu;

function canonicalRepositoryKey(key: string): string | null {
  const repository = ALT_SSH_HOST_KEY.exec(key)?.[1];
  return repository === undefined ? null : `gitlab.com/${repository}`;
}

function repositoryKeyFromRemote(key: string): string | null {
  return canonicalRepositoryKey(key)?.replace(/\.git$/u, "") ?? null;
}

// Saved identities reach the predicates as written, so a key from before this mapping still matches.
function withCanonicalKey<Identity extends { readonly canonicalKey: string }>(
  identity: Identity,
): Identity {
  const canonicalKey = canonicalRepositoryKey(identity.canonicalKey);
  return canonicalKey === null ? identity : { ...identity, canonicalKey };
}

// Its port, if given, is the SSH one, not where the web pages are.
function webHost(host: string): string {
  return /^altssh\.gitlab\.com(?::\d+)?$/iu.test(host) ? "gitlab.com" : host;
}

export const definition = defineSourceControl({
  kind: KIND,
  label: "GitLab",
  pickerLabel: "GitLab",
  icon,
  changeRequest: {
    shortLabel: "MR",
    shortLabelWithArticle: "an MR",
    singular: "merge request",
    plural: "merge requests",
  },
  repositoryPathHint: "group/project",
  publicHost: "gitlab.com",
  publishDescription: "gitlab.com",
  publishHost: () => "gitlab.com",
  newRepositoryOwner: (account) => ({ owner: account }),
  defaultCloneTransport: "ssh",
  changeRequestUrl: ({ host, repository, number, remoteUrl, webUrl }) => {
    if (!Number.isSafeInteger(number) || number < 1) return null;
    if (webUrl) return changeRequestUrlBelow(webUrl, `-/merge_requests/${number}`);
    const web = webHost(host);
    if (!isProjectPath(repository, web)) return null;
    try {
      return new URL(
        `/${repository}/-/merge_requests/${number}`,
        remoteWebOrigin(web, remoteUrl),
      ).toString();
    } catch {
      return null;
    }
  },
  parseChangeRequestUrl: (url) => parseChangeRequestPath(url, CHANGE_REQUEST_PATH),
  parseIssueUrl: (url) => parseIssuePath(url, ISSUE_PATH),
  commitUrl: (input) => buildCommitUrl({ ...input, host: webHost(input.host) }, "-/commit"),
  parseCommitUrl: (url) => parseCommitPath(url, COMMIT_PATH),
  repositoryUrlFromPage: (url) => repositoryUrlFromPagePath(url, REPOSITORY_PAGE_PATH),
  // `!12` already links the merge request itself; `#12` is always an issue.
  changeRequestUrlFromReferenceAutolink: (url) =>
    CHANGE_REQUEST_PATH.test(url.pathname) ? url.toString() : null,
  changeRequestActions: new Set([
    "merge",
    "ready",
    "draft",
    "close",
    "reopen",
    "update-branch",
    "enable-auto-merge",
    "disable-auto-merge",
    "revert",
    "delete-branch",
  ] as const),
  checkoutCommand: ({ number }) => `glab mr checkout ${number}`,
  // `accept` is glab's alias for `merge`.
  changeRequestCloseCommand: /\bglab\s+mr\s+(?:merge|accept|close)\b/u,
  // `new` is glab's alias for `create`.
  changeRequestCreateCommand: /\bglab\s+mr\s+(?:create|new)\b/u,
  // Bot users, unlike GitHub's, have real profile pages.
  authorProfileUrl: (login, repositoryUrl) =>
    new URL(`/${encodeURIComponent(login)}`, repositoryUrl).toString(),
  referenceAutolinks: gitLabReferenceAutolinks,
  reviewSummaryRequired: () => false,
  checkoutCommandArgument: (input) => CHECKOUT_COMMAND.exec(input)?.[1]?.trim() ?? null,
  isChangeRequestReference: (url) => CHANGE_REQUEST_REFERENCE.test(url),
  checkoutChangeRequestHost: () => null,
  changeRequestHeadRef: (number) => `refs/merge-requests/${number}/head`,
  recognizeRemote: ({ host, hostname }) =>
    hostname === "altssh.gitlab.com"
      ? { kind: KIND, name: "GitLab", baseUrl: "https://gitlab.com" }
      : hostname === "gitlab.com" || hostname.split(".").includes("gitlab")
        ? {
            kind: KIND,
            name: hostname === "gitlab.com" ? "GitLab" : "GitLab Self-Hosted",
            baseUrl: `https://${host}`,
          }
        : null,
  repositoryKeyFromRemote,
  canonicalRepositoryKey,
  isChangeRequestInRepository: (saved, link) => {
    const identity = withCanonicalKey(saved);
    return isChangeRequestInProjectRepository(
      projectChangeRequestHost(identity, KIND),
      identity,
      link,
    );
  },
  canReadChangeRequestOnHost: (saved, link) => {
    const identity = withCanonicalKey(saved);
    return isChangeRequestOnProjectHost(projectChangeRequestHost(identity, KIND), identity, link);
  },
});
