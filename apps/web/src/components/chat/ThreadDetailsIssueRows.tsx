import { scopedThreadKey } from "@onicode/client-runtime/environment";
import { threadIssueKeyOf, type ScopedThreadRef, type ThreadIssueLink } from "@onicode/contracts";
import { T, useGT, Var } from "gt-react";
import { ChevronDownIcon, ChevronUpIcon, MessageSquareIcon } from "lucide-react";
import { memo, useState, type MouseEvent } from "react";

import { cn } from "~/lib/utils";
import { useRightPanelStore } from "~/rightPanelStore";
import { useServerConfigs } from "~/state/entities";
import { useLinkedIssueSummary } from "~/state/issues";

import {
  IssueGlyph,
  issueStatePresentation,
  issueTrackerPresentation,
} from "../issue/issuePresentation";
import {
  PULL_REQUEST_ROW_CLASS,
  PULL_REQUEST_ROW_NUMBER_CLASS,
  PullRequestRowLines,
} from "../pullRequest/PullRequestListRow";
import { PullRequestActorLabel } from "../pullRequest/pullRequestPresentation";
import { ThreadDetailsControl } from "./ThreadDetailsControl";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

type IssueRowMountScope = "thread-details" | "linked-items" | "sidebar-tooltip";

function LinkedIssueRowImpl({
  link,
  threadRef,
  compact,
  mountScope,
  onOpen,
}: {
  link: ThreadIssueLink;
  threadRef: ScopedThreadRef;
  compact: boolean;
  mountScope: IssueRowMountScope;
  onOpen: (() => void) | undefined;
}) {
  const gt = useGT();
  const configs = useServerConfigs();
  const detail = useLinkedIssueSummary(threadRef.environmentId, link, mountScope);
  const trackerPresentation = issueTrackerPresentation(link.tracker);
  const state = detail === null ? null : issueStatePresentation(detail.state);
  const Icon = state?.Icon ?? IssueGlyph.issue;
  const title = detail?.title ?? link.title;
  const anchorProps = {
    href: link.url,
    target: "_blank",
    rel: "noopener noreferrer",
    "aria-label": gt("Issue {key}: {title}", { key: link.displayKey, title }),
    onClick: (event: MouseEvent<HTMLElement>) => {
      event.stopPropagation();
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      if (configs.get(threadRef.environmentId)?.environment.capabilities.issues !== true) return;
      event.preventDefault();
      useRightPanelStore.getState().openIssue(threadRef, link);
      onOpen?.();
    },
  };
  const glyph = (
    <Icon
      aria-hidden={state === null ? true : undefined}
      role={state === null ? undefined : "img"}
      aria-label={state?.label}
      className={cn("size-4 shrink-0", state?.toneClassName ?? "text-muted-foreground")}
    />
  );
  const comments =
    detail !== null && detail.commentCount > 0 ? (
      <span
        role="img"
        aria-label={gt("{count, plural, one {{count} comment} other {{count} comments}}", {
          count: detail.commentCount,
        })}
        className="inline-flex shrink-0 items-center gap-0.5 tabular-nums"
      >
        <MessageSquareIcon aria-hidden className="size-3" />
        <span aria-hidden>{detail.commentCount}</span>
      </span>
    ) : null;
  const trackerIcon = (
    <trackerPresentation.Icon
      role="img"
      aria-label={trackerPresentation.providerName}
      className="size-3 shrink-0"
    />
  );
  const lines = (
    <>
      {glyph}
      <PullRequestRowLines
        number={<span className={PULL_REQUEST_ROW_NUMBER_CLASS}>{link.displayKey}</span>}
        title={title}
        status={comments}
        meta={
          <>
            {detail?.author ? (
              <PullRequestActorLabel actor={detail.author} tooltip={false} />
            ) : null}
            {trackerIcon}
            <span className="truncate">{link.scope}</span>
          </>
        }
        updatedAt={detail?.updatedAt ?? link.linkedAt}
      />
    </>
  );
  return compact ? (
    <div className="pb-1.5">
      <Tooltip>
        <TooltipTrigger
          render={<ThreadDetailsControl part="title" render={<a {...anchorProps} />} />}
        >
          {glyph}
          <span className="min-w-0 flex-1 line-clamp-2 whitespace-normal leading-4.5">{title}</span>
        </TooltipTrigger>
        <TooltipPopup
          side="top"
          align="start"
          variant="glass"
          className="w-80 max-w-[calc(100vw-2rem)]"
        >
          <div className="flex min-w-0 items-center gap-2">{lines}</div>
        </TooltipPopup>
      </Tooltip>
      <div className="flex h-5 min-w-0 items-center gap-1.5 ps-9 pe-2.5 text-xs text-muted-foreground">
        <span className="shrink-0 tabular-nums">{link.displayKey}</span>
        {trackerIcon}
        {comments}
      </div>
    </div>
  ) : (
    <a
      {...anchorProps}
      className={cn(
        PULL_REQUEST_ROW_CLASS,
        "px-2 hover:bg-accent/60 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring",
      )}
    >
      {lines}
    </a>
  );
}

export const LinkedIssueRow = memo(LinkedIssueRowImpl);

// Each row reads its own summary; agents can link dozens of issues to one thread.
const COLLAPSED_ISSUE_ROWS = 3;

export function ThreadDetailsIssueRows({
  links,
  threadRef,
  compact = true,
  mountScope = "thread-details",
  onOpen,
}: {
  links: ReadonlyArray<ThreadIssueLink>;
  threadRef: ScopedThreadRef;
  compact?: boolean;
  mountScope?: Exclude<IssueRowMountScope, "linked-items">;
  onOpen?: () => void;
}) {
  // Keyed by thread: the details card stays mounted when another thread opens.
  const threadKey = scopedThreadKey(threadRef);
  const [expandedThreadKey, setExpandedThreadKey] = useState<string | null>(null);
  const expanded = expandedThreadKey === threadKey;
  const hidden = links.length - COLLAPSED_ISSUE_ROWS;
  const row = (link: ThreadIssueLink) => (
    <LinkedIssueRow
      // Links made before keys compared case-insensitively may repeat an issue.
      key={`${threadIssueKeyOf(link)} ${link.key}`}
      link={link}
      threadRef={threadRef}
      compact={compact}
      mountScope={mountScope}
      onOpen={onOpen}
    />
  );
  return (
    <>
      {links.slice(0, COLLAPSED_ISSUE_ROWS).map(row)}
      {expanded ? (
        <div data-thread-details-issue-list className="flex flex-col">
          {links.slice(COLLAPSED_ISSUE_ROWS).map(row)}
        </div>
      ) : null}
      {hidden > 0 ? (
        <ThreadDetailsControl
          variant="ghost"
          size="sm"
          aria-expanded={expanded}
          onClick={(event) => {
            // The sidebar badge's preview is portaled inside its thread row, which opens on click.
            event.stopPropagation();
            setExpandedThreadKey(expanded ? null : threadKey);
          }}
          part="row"
          tone="muted"
          className="w-full active:scale-100"
        >
          {expanded ? (
            <ChevronUpIcon aria-hidden className="size-4 shrink-0" />
          ) : (
            <ChevronDownIcon aria-hidden className="size-4 shrink-0" />
          )}
          {expanded ? (
            <T>Show less</T>
          ) : (
            <T>
              Show <Var>{hidden}</Var> more
            </T>
          )}
        </ThreadDetailsControl>
      ) : null}
    </>
  );
}
