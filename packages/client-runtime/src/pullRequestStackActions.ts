/**
 * Where the host auto-merges stacks, a stacked layer's auto-merge request is the stack's, which
 * only the stack's cancel reaches; until membership is known the layer may be stacked.
 */
export function allowsSinglePullRequestAutoMergeCancel(input: {
  readonly stackAutoMerge: boolean;
  readonly canMergeSinglePullRequest: boolean;
}): boolean {
  return !input.stackAutoMerge || input.canMergeSinglePullRequest;
}
