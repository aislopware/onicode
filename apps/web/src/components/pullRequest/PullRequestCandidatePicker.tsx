// Menu typeahead consumes search keystrokes; use a combobox.
import type { ReactNode } from "react";

import { Button } from "../ui/button";
import {
  Combobox,
  ComboboxSearchInput,
  ComboboxItem,
  ComboboxList,
  ComboboxPopup,
  ComboboxTrigger,
} from "../ui/combobox";
import { PullRequestPeopleGhost } from "./PullRequestGhosts";
import { SharedTooltipTrigger } from "../ui/shared-tooltip";

export function PullRequestCandidatePicker<T>({
  icon,
  label,
  allowed,
  disabledReason,
  open,
  onOpenChange,
  query,
  onQueryChange,
  searchLabel,
  isPending,
  error,
  candidates,
  emptyLabel,
  noMatchLabel,
  errorLabel,
  errorAction,
  truncated,
  truncatedLabel,
  truncatedAction,
  candidateKey,
  onSelect,
  children,
}: {
  icon: ReactNode;
  label: string;
  allowed: boolean;
  disabledReason: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  query: string;
  onQueryChange: (query: string) => void;
  searchLabel: string;
  isPending: boolean;
  error: string | null;
  candidates: ReadonlyArray<T>;
  emptyLabel: string;
  noMatchLabel: string;
  errorLabel: string;
  errorAction?: ReactNode;
  truncated: boolean;
  truncatedLabel: string;
  truncatedAction?: ReactNode;
  candidateKey: (candidate: T) => string;
  onSelect: (candidate: T) => void;
  children: (candidate: T) => ReactNode;
}) {
  if (!allowed) {
    return (
      <SharedTooltipTrigger
        render={
          <Button size="icon-xs" variant="ghost" aria-disabled="true" aria-label={label}>
            {icon}
          </Button>
        }
        label={disabledReason}
        side="bottom"
      />
    );
  }

  const keys = candidates.map(candidateKey);

  return (
    <Combobox
      items={keys}
      filteredItems={keys}
      filter={null}
      autoHighlight
      value={null}
      onValueChange={(key) => {
        const candidate = candidates.find((entry) => candidateKey(entry) === key);
        if (candidate) onSelect(candidate);
      }}
      open={open}
      onOpenChange={(nextOpen, details) => {
        // Item-press closure steals search focus and the next keystroke.
        if (!nextOpen && details.reason === "item-press") {
          details.cancel();
          return;
        }
        onOpenChange(nextOpen);
      }}
    >
      <ComboboxTrigger
        render={
          <Button size="icon-xs" variant="ghost" aria-label={label}>
            {icon}
          </Button>
        }
      />
      <ComboboxPopup align="start" side="bottom" className="w-72">
        <ComboboxSearchInput
          aria-label={searchLabel}
          placeholder={searchLabel}
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
        />
        <ComboboxList scrollAreaClassName="max-h-72">
          {isPending ? (
            <PullRequestPeopleGhost rows={4} />
          ) : error !== null ? (
            <div className="flex items-center gap-2 p-2">
              <p className="min-w-0 flex-1 text-xs text-muted-foreground">
                {errorLabel} {error}
              </p>
              {errorAction}
            </div>
          ) : candidates.length === 0 ? (
            <p className="p-2 text-xs text-muted-foreground">
              {query.length > 0 ? noMatchLabel : emptyLabel}
            </p>
          ) : (
            candidates.map((candidate, index) => (
              <ComboboxItem key={keys[index]} hideIndicator index={index} value={keys[index]}>
                {children(candidate)}
              </ComboboxItem>
            ))
          )}
          {truncated ? (
            <div className="flex items-center gap-2 px-2 py-1.5">
              <p className="min-w-0 flex-1 text-xs text-muted-foreground">{truncatedLabel}</p>
              {truncatedAction}
            </div>
          ) : null}
        </ComboboxList>
      </ComboboxPopup>
    </Combobox>
  );
}
