import * as AzureDevOpsDriver from "@onicode/source-control-azure-devops/server/driver";
import * as BitbucketDriver from "@onicode/source-control-bitbucket/server/driver";
import * as CursorOriginDriver from "@onicode/source-control-cursor-origin/server/driver";
import { parseGitRemoteHost } from "@onicode/source-control-core/shared/definition";
import { builtInSourceControlDefinitions } from "@onicode/source-control-definitions/registry";
import * as ForgejoDriver from "@onicode/source-control-forgejo/server/driver";
import * as GitCafeDriver from "@onicode/source-control-gitcafe/server/driver";
import * as GitHubDriver from "@onicode/source-control-github/server/driver";
import * as GitLabDriver from "@onicode/source-control-gitlab/server/driver";
import * as TanStackSourceDriver from "@onicode/source-control-tanstack-source/server/driver";

/**
 * In the shared registry's order, which is the order hosts are listed and probed in. Feature
 * flags leave some out; see `builtInDrivers.ts`.
 */
export const BUILT_IN_SOURCE_CONTROL_DRIVER_LIST = [
  GitHubDriver.driver,
  CursorOriginDriver.driver,
  GitLabDriver.driver,
  GitCafeDriver.driver,
  TanStackSourceDriver.driver,
  ForgejoDriver.driver,
  BitbucketDriver.driver,
  AzureDevOpsDriver.driver,
];

export const sourceControlDefinitions = builtInSourceControlDefinitions;

const namesForge = (remoteUrl: string) =>
  (sourceControlDefinitions.recognizeRemote(remoteUrl)?.kind ?? "unknown") !== "unknown";

/** A remote on a forge's public host, such as `git@github.com:o/r`. */
function isPublicForgeRemote(remoteUrl: string): boolean {
  const hostname = parseGitRemoteHost(remoteUrl)?.hostname;
  return (
    hostname !== undefined &&
    sourceControlDefinitions.findByPublicHost(hostname) !== undefined &&
    namesForge(remoteUrl)
  );
}

/** Whether `preferredRemoteUrl` can pick a remote's configured URL over this fetch URL. */
export const configuredRemoteUrlMayWin = (fetchUrl: string) => !isPublicForgeRemote(fetchUrl);

/** Each remote's configured URL from `git config --get-regexp` output, before url.*.insteadOf. */
export function parseConfiguredRemoteUrls(config: string): Map<string, string> {
  const urls = new Map<string, string>();
  for (const line of config.split("\n")) {
    const match = /^remote\.(\S+)\.url (.+)$/u.exec(line);
    // Git fetches from the first of several configured URLs.
    if (match?.[1] && match[2] && !urls.has(match[1])) urls.set(match[1], match[2]);
  }
  return urls;
}

/**
 * Which of a remote's configured URL and its `url.*.insteadOf` fetch URL (a proxy, mirror, SSH
 * alias like `github.work`, or expanded shorthand like `gh:`) names its repository. A forge's label
 * matches any host carrying it, so over a fetch URL naming a forge only a public host wins.
 */
export function preferredRemoteUrl(configuredUrl: string, fetchUrl: string): string {
  if (!configuredRemoteUrlMayWin(fetchUrl) || !namesForge(configuredUrl)) return fetchUrl;
  return !namesForge(fetchUrl) || isPublicForgeRemote(configuredUrl) ? configuredUrl : fetchUrl;
}

/** A host CLI command that merges or closes a change request, such as `gh pr merge`. */
export const isChangeRequestCloseCommand = (command: string) =>
  sourceControlDefinitions.definitions.some(
    (definition) => definition.changeRequestCloseCommand?.test(command) === true,
  );

// A quoted or bare shell word. An unclosed quote, as Codex's `/bin/zsh -lc "gh pr merge 19"`
// leaves after the match, runs to the end. A closed one right after `=` stays in that word and is
// unquoted before matching, so `--id="19"` names 19 and `--subject="20"` names none, while the
// wrapper's closing quote in `-lc "gh pr merge 19" > "log"` still ends the word `19`.
const SHELL_WORD =
  /"((?:\\[\s\S]|[^"\\])*)"?|'([^']*)'?|((?:\\[\s\S]|[^\s"'\\])+(?:(?<==)(?:"(?:\\[\s\S]|[^"\\])*"|'[^']*'))?)/gu;
// A whole word that names one, so a number inside `--subject "Release 20"` does not. Of the
// hosts' close commands only Azure's selects one with a flag, so `--subject=2026` and
// `--body=<pull request URL>` are option text.
const NAMED_CHANGE_REQUEST = /^(?:--id=)?#?(\d+)$|^(?!-)\S*\/(?:pulls?|merge_requests)\/(\d+)\/?$/u;

/**
 * The change request numbers a merge or close command names, or "any" when one names none, as a
 * bare `gh pr merge` on the checked-out branch or `gh stack merge` does.
 */
export function changeRequestsClosedBy(command: string): ReadonlySet<number> | "any" {
  const named = new Set<number>();
  for (const { changeRequestCloseCommand: close } of sourceControlDefinitions.definitions) {
    if (close === undefined) continue;
    for (const match of command.matchAll(new RegExp(close.source, `${close.flags}g`))) {
      // From the match's start, since Azure's match runs through `--id` to `--status`.
      const args = command.slice(match.index).split(/[;&|\n]/u, 1)[0] ?? "";
      // Every host's merge and close takes one change request, so any later one is option text.
      let target: string | undefined;
      for (const [, double, single, bare] of args.matchAll(SHELL_WORD)) {
        // A comment ends the command.
        if (bare !== undefined && /^#(?!\d+$)/u.test(bare)) break;
        const word = double ?? single ?? bare?.replace(/\\([\s\S])|["']/gu, "$1") ?? "";
        const [, number, url] = NAMED_CHANGE_REQUEST.exec(word) ?? [];
        target = number ?? url;
        if (target !== undefined) break;
      }
      if (target === undefined) return "any";
      named.add(Number(target));
    }
  }
  return named.size === 0 ? "any" : named;
}
