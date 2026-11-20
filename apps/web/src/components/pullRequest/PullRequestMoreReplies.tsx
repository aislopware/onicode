import type { ScopedThreadRef } from "@onicode/contracts";
import { T, useGT } from "gt-react";

import { anchorOpenHandlers, useOpenLink } from "~/browser/useOpenLink";

import type { PullRequestReplyAvailability } from "./pullRequestDetail.logic";

export function PullRequestMoreReplies({
  moreReplies,
  hasMoreReplies,
  url,
  threadRef,
}: PullRequestReplyAvailability & { readonly threadRef: ScopedThreadRef | null }) {
  const gt = useGT();
  const openLink = useOpenLink(threadRef);
  if (!hasMoreReplies) return null;
  return (
    <div className="flex flex-wrap items-baseline gap-2 py-1 text-xs text-muted-foreground">
      <span>
        {moreReplies > 0
          ? gt("{count, plural, one {{formatted} more reply} other {{formatted} more replies}}", {
              count: moreReplies,
              formatted: moreReplies.toLocaleString(),
            })
          : gt("More replies")}
      </span>
      {url ? (
        <a
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          className="hover:text-foreground hover:underline"
          {...anchorOpenHandlers(openLink, url)}
        >
          <T>View on host</T>
        </a>
      ) : null}
    </div>
  );
}
