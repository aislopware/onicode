import * as AzureDevOpsDriver from "@onicode/source-control-azure-devops/server/driver";
import * as BitbucketDriver from "@onicode/source-control-bitbucket/server/driver";
import * as CursorOriginDriver from "@onicode/source-control-cursor-origin/server/driver";
import { builtInSourceControlDefinitions } from "@onicode/source-control-definitions/registry";
import * as ForgejoDriver from "@onicode/source-control-forgejo/server/driver";
import * as GitCafeDriver from "@onicode/source-control-gitcafe/server/driver";
import * as GitHubDriver from "@onicode/source-control-github/server/driver";
import * as GitLabDriver from "@onicode/source-control-gitlab/server/driver";

/** In the shared registry's order, which is the order hosts are listed and probed in. */
export const BUILT_IN_SOURCE_CONTROL_DRIVER_LIST = [
  GitHubDriver.driver,
  CursorOriginDriver.driver,
  GitLabDriver.driver,
  GitCafeDriver.driver,
  ForgejoDriver.driver,
  BitbucketDriver.driver,
  AzureDevOpsDriver.driver,
] as const;

export const sourceControlDefinitions = builtInSourceControlDefinitions;
