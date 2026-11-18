import type { PreviewCard as PreviewCardPrimitive } from "@base-ui/react/preview-card";
import type { EnvironmentId, IssueRef, IssueSummary } from "@onicode/contracts";
import { T, Var } from "gt-react";
import { useRef, useState, type ComponentPropsWithoutRef, type ReactElement } from "react";

import { RelativeTime } from "~/components/RelativeTime";
import { issueEnvironment } from "~/state/issues";
import { useEnvironmentQuery } from "~/state/query";

import { PullRequestActorAvatar } from "../pullRequest/pullRequestPresentation";
import { PreviewCard, PreviewCardPopup, PreviewCardTrigger } from "../ui/preview-card";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { issueStatePresentation } from "./issuePresentation";

export interface IssueLinkPreviewTarget {
  readonly environmentId: EnvironmentId;
  readonly input: IssueRef;
}

export function IssueLinkPreview({
  link,
  originalUrl,
  target,
}: {
  link: ReactElement<ComponentPropsWithoutRef<"a">>;
  originalUrl: string;
  target: IssueLinkPreviewTarget;
}) {
  const [open, setOpen] = useState(false);
  const previewActionsRef = useRef<PreviewCardPrimitive.Root.Actions | null>(null);
  const summaryQuery = useEnvironmentQuery(open ? issueEnvironment.summary(target) : null);
  const summary = summaryQuery.data;
  const showUrlTooltip = open && summaryQuery.error !== null && summary === null;

  return (
    <PreviewCard open={open} onOpenChange={setOpen} actionsRef={previewActionsRef}>
      <Tooltip
        open={showUrlTooltip}
        onOpenChange={(nextOpen) => {
          if (!nextOpen && summary === null) previewActionsRef.current?.close();
        }}
      >
        <PreviewCardTrigger
          render={<TooltipTrigger render={link} />}
          delay={350}
          closeDelay={120}
        />
        <TooltipPopup side="top">{originalUrl}</TooltipPopup>
      </Tooltip>
      {summary === null ? null : (
        <PreviewCardPopup align="center" className="w-80 max-w-[calc(100vw-2rem)]">
          <div className="p-3">
            <IssuePreviewCardBody summary={summary} />
          </div>
        </PreviewCardPopup>
      )}
    </PreviewCard>
  );
}

/** A pull request card's fallback when its reference turns out to be an issue. */
export function IssueLinkPreviewFallback({
  originalUrl,
  target,
}: {
  originalUrl: string;
  target: IssueLinkPreviewTarget;
}) {
  const summary = useEnvironmentQuery(issueEnvironment.summary(target)).data;
  return summary === null ? (
    <p className="break-all text-xs text-muted-foreground">{originalUrl}</p>
  ) : (
    <IssuePreviewCardBody summary={summary} />
  );
}

function IssuePreviewCardBody({ summary }: { summary: IssueSummary }) {
  const state = issueStatePresentation(summary.state);
  const createdAt = <RelativeTime timestamp={summary.createdAt} />;
  const author = summary.author;
  const authorLabel =
    author === null
      ? "ghost"
      : author.name && author.name !== author.login
        ? `${author.name} (@${author.login})`
        : author.login;
  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-center gap-1.5 text-2xs text-muted-foreground">
        <span className="min-w-0 truncate">{summary.scope}</span>
        <span className="shrink-0">{summary.displayKey}</span>
        <span aria-hidden>·</span>
        <span className="inline-flex shrink-0 items-center gap-1">
          <state.Icon aria-hidden className={`size-3 ${state.toneClassName}`} />
          {state.label}
        </span>
      </div>
      <p className="mt-1 text-sm font-medium leading-snug text-foreground text-pretty">
        {summary.title}
      </p>
      <div className="mt-2 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <PullRequestActorAvatar actor={author} className="size-4" />
        <span className="min-w-0 truncate">{authorLabel}</span>
        <span aria-hidden>·</span>
        <span className="shrink-0">
          <T>
            opened <Var>{createdAt}</Var>
          </T>
        </span>
      </div>
    </div>
  );
}
