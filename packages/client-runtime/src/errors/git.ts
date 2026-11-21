import { GitCommandError, type GitCommandFailureReason } from "@onicode/contracts";
import * as Schema from "effect/Schema";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

const isGitCommandError = Schema.is(GitCommandError);

const GIT_FAILURE_REASON_MESSAGES = {
  authentication_failed: msg(
    "Git could not sign in to the remote. Check your credentials, then try again.",
  ),
  branch_already_exists: msg("A branch with this name already exists."),
  branch_checked_out_in_worktree: msg("This branch is already checked out in another worktree."),
  config_locked: msg(
    "Another Git process kept the repository's config locked. Wait for it to finish, then retry.",
  ),
  host_key_unverified: msg(
    "This machine does not trust the remote's host key yet. Connect to it once from a terminal to verify it.",
  ),
  local_changes_would_be_overwritten: msg(
    "Your local changes would be overwritten by switching branches. Commit, stash, or discard them, then switch again.",
  ),
  not_a_repository: msg("This folder is not a Git repository."),
  path_already_exists: msg("Something already exists at that path."),
  push_rejected_by_remote: msg(
    "The remote rejected the push. Push this branch from a terminal to see the server's message.",
  ),
  push_rejected_non_fast_forward: msg(
    "The remote has commits this branch doesn't have. Pull or rebase, then push again.",
  ),
  ref_name_conflict: msg("This name conflicts with an existing branch or tag."),
  ref_not_found: msg("That branch or commit does not exist."),
  remote_unreachable: msg(
    "Could not reach the remote repository. Check its address and your network, then try again.",
  ),
  tag_would_be_clobbered: msg("A tag with this name already exists and would be overwritten."),
} satisfies Record<GitCommandFailureReason, string>;

/**
 * A Git failure in the active language when its reason is known, else its sanitized detail; never
 * the message's operation and cwd. Null for anything that is not a Git failure.
 */
export function gitCommandErrorMessage(
  error: unknown,
  resolve: MFunctionType = mFallback,
): string | null {
  if (!isGitCommandError(error)) return null;
  return error.reason === undefined
    ? error.detail
    : resolve(GIT_FAILURE_REASON_MESSAGES[error.reason]);
}
